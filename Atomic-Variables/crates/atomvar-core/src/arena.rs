//! Named shared-memory arenas: bootstrap protocol, name registry, fork policy.

use crate::algo::{self, Inserted, Probe, SlotTable, READY};
use crate::error::{Error, Result};
use crate::handles::{AnyAtomic, Owner};
use crate::hooks;
use crate::layout::*;
use crate::value::{Value, ValueType};
use memmap2::{MmapOptions, MmapRaw};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering as O};
use std::sync::{Arc, Mutex, OnceLock, Weak};

pub const DEFAULT_CAPACITY: u32 = 1024;
pub const MAX_CAPACITY: u32 = 1 << 20;
const SHM_DIR: &str = "/dev/shm";
const SHM_PREFIX: &str = "atomvar-";

// ---------------------------------------------------------------------------
// Fork policy: fail fast, never recover
// ---------------------------------------------------------------------------

/// pid of the process that first used the arena machinery; 0 = not yet.
static INIT_PID: AtomicU32 = AtomicU32::new(0);

/// Must run before touching any mutex, fd or allocation on registry paths. In a
/// child created by plain fork() the inherited locks may be held by threads that
/// no longer exist, so the child is refused instead of "recovered".
#[inline]
fn check_pid() -> Result<()> {
    // SAFETY: getpid is always safe.
    let me = unsafe { libc::getpid() } as u32;
    match INIT_PID.compare_exchange(0, me, O::AcqRel, O::Acquire) {
        Ok(_) => Ok(()),
        Err(owner) if owner == me => Ok(()),
        Err(_) => Err(Error::ForkedProcess),
    }
}

// ---------------------------------------------------------------------------
// flock guard
// ---------------------------------------------------------------------------

/// Exclusive flock on an open file description. NOTE: flock does not exclude
/// threads sharing the same fd, so registry writers also take a process mutex.
struct Flock<'a>(&'a File);

impl<'a> Flock<'a> {
    fn exclusive(f: &'a File) -> Result<Self> {
        loop {
            // SAFETY: valid fd owned by `f`.
            let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if r == 0 {
                return Ok(Self(f));
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e.into());
            }
        }
    }
}

impl Drop for Flock<'_> {
    fn drop(&mut self) {
        // SAFETY: valid fd.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

// ---------------------------------------------------------------------------
// Arena
// ---------------------------------------------------------------------------

pub(crate) struct ArenaInner {
    name: String,
    /// Kept open for registry locking. O_CLOEXEC, so exec'd children never inherit it.
    file: File,
    _map: MmapRaw,
    base: NonNull<u8>,
    capacity: u32,
    /// Serializes registry inserts between threads of this process.
    insert_lock: Mutex<()>,
}

// SAFETY: all shared-memory access goes through atomics, or through payload
// writes serialized by `insert_lock` + flock (see algo::SlotTable).
unsafe impl Send for ArenaInner {}
unsafe impl Sync for ArenaInner {}

impl ArenaInner {
    fn slot(&self, i: usize) -> &Slot {
        debug_assert!(i < self.capacity as usize);
        // SAFETY: i < capacity, mapping covers SLOTS_OFFSET + capacity * SLOT_SIZE.
        unsafe { &*(self.base.as_ptr().add(SLOTS_OFFSET + i * SLOT_SIZE) as *const Slot) }
    }

    pub(crate) fn value_ptr(&self, i: usize) -> NonNull<u8> {
        NonNull::new(self.slot(i).value.0.get() as *mut u8).expect("non-null mapping")
    }
}

impl SlotTable for ArenaInner {
    fn capacity(&self) -> usize {
        self.capacity as usize
    }

    fn load_state(&self, i: usize, o: O) -> u32 {
        self.slot(i).state.load(o)
    }

    fn store_state(&self, i: usize, v: u32, o: O) {
        self.slot(i).state.store(v, o)
    }

    unsafe fn name_eq(&self, i: usize, name: &[u8]) -> bool {
        let s = self.slot(i);
        let len = *s.name_len.get() as usize;
        let stored: &[u8; MAX_VAR_NAME_LEN] = &*s.name.get();
        len == name.len() && &stored[..len] == name
    }

    unsafe fn type_tag(&self, i: usize) -> u8 {
        *self.slot(i).type_tag.get()
    }

    unsafe fn write_payload(&self, i: usize, type_tag: u8, name: &[u8], init: u128) {
        let s = self.slot(i);
        let n = &mut *s.name.get();
        n.fill(0);
        n[..name.len()].copy_from_slice(name);
        *s.name_len.get() = name.len() as u8;
        *s.type_tag.get() = type_tag;
        // Atomic store of the right width: no mixed-size non-atomic init.
        let p = self.value_ptr(i).as_ptr();
        match ValueType::from_tag(type_tag).expect("valid tag") {
            ValueType::I64 | ValueType::U64 | ValueType::F64 => {
                std::sync::atomic::AtomicU64::from_ptr(p as *mut u64).store(init as u64, O::Relaxed)
            }
            ValueType::Bool => {
                std::sync::atomic::AtomicBool::from_ptr(p as *mut bool).store(init != 0, O::Relaxed)
            }
            ValueType::U128 => portable_atomic::AtomicU128::from_ptr(p as *mut u128)
                .store(init, O::Relaxed),
        }
        hooks::point("after_payload_before_ready");
    }
}

/// Handle to a named shared-memory arena. Cheap to clone; every handle and every
/// variable handle keeps the mapping alive.
#[derive(Clone)]
pub struct Arena {
    inner: Arc<ArenaInner>,
}

impl std::fmt::Debug for Arena {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Arena")
            .field("name", &self.inner.name)
            .field("capacity", &self.inner.capacity)
            .finish()
    }
}

/// A published variable, as reported by `Arena::list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarInfo {
    pub name: String,
    pub value_type: ValueType,
    pub slot: u32,
}

fn arenas() -> &'static Mutex<HashMap<String, Weak<ArenaInner>>> {
    static ARENAS: OnceLock<Mutex<HashMap<String, Weak<ArenaInner>>>> = OnceLock::new();
    ARENAS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Checks an arena name without touching the filesystem.
pub fn validate_arena_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_ARENA_NAME_LEN
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidName(format!(
            "arena name must be 1-{MAX_ARENA_NAME_LEN} chars of [A-Za-z0-9._-], not starting with '.': {name:?}"
        )))
    }
}

fn validate_var_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_VAR_NAME_LEN || name.contains('\0') {
        return Err(Error::InvalidName(format!(
            "variable name must be 1-{MAX_VAR_NAME_LEN} bytes of UTF-8 without NUL: {name:?}"
        )));
    }
    Ok(())
}

fn shm_path(name: &str) -> PathBuf {
    PathBuf::from(SHM_DIR).join(format!("{SHM_PREFIX}{name}"))
}

/// Capacity implied by a segment size, if the size is one this layout can have.
fn capacity_for_size(size: u64) -> Option<u32> {
    let slots = size.checked_sub(SLOTS_OFFSET as u64)?;
    if slots == 0 || slots % SLOT_SIZE as u64 != 0 {
        return None;
    }
    let cap = slots / SLOT_SIZE as u64;
    (cap <= MAX_CAPACITY as u64).then_some(cap as u32)
}

/// Checks that a zero-signature segment is exactly what an interrupted creator
/// of this protocol leaves behind. Caller holds the exclusive flock.
fn unpublished_state_is_ours(map: &MmapRaw, size: u64) -> std::result::Result<(), String> {
    let cap = capacity_for_size(size).ok_or_else(|| format!("{size} bytes is not a valid arena size"))?;
    // SAFETY: the mapping is `size` >= SLOTS_OFFSET bytes; exclusive flock held
    // and the segment is unpublished, so nobody else accesses it.
    let bytes = unsafe { std::slice::from_raw_parts(map.as_mut_ptr() as *const u8, size as usize) };
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
    let ok = |v: u64, want: u64| v == 0 || v == want;
    if !ok(u32_at(8) as u64, VERSION as u64)
        || !ok(u32_at(12) as u64, cap as u64)
        || !ok(u32_at(16) as u64, SLOT_SIZE as u64)
        || u32_at(20) != 0
        || !ok(u64_at(24), LAYOUT_HASH)
    {
        return Err("its header fields are not ones this layout writes".into());
    }
    if bytes[32..].iter().any(|&b| b != 0) {
        return Err("it has non-zero data past the header".into());
    }
    Ok(())
}

fn map_len(file: &File, len: u64) -> Result<MmapRaw> {
    Ok(MmapOptions::new().len(len as usize).map_raw(file)?)
}

/// Whether an arena segment with this name exists (it may still be unpublished).
pub fn arena_exists(name: &str) -> Result<bool> {
    validate_arena_name(name)?;
    Ok(shm_path(name).exists())
}

/// Removes an arena's segment. ADMIN ONLY: processes that still have it mapped
/// keep using the orphaned segment while new openers get a fresh one (split
/// brain). Never run while the arena is in use.
pub fn destroy_arena(name: &str) -> Result<()> {
    check_pid()?;
    validate_arena_name(name)?;
    std::fs::remove_file(shm_path(name))?;
    if let Ok(mut m) = arenas().lock() {
        m.remove(name);
    }
    Ok(())
}

impl Arena {
    /// Opens (creating if needed) the arena `name`.
    ///
    /// `capacity`: `None` adopts an existing arena's capacity or creates one with
    /// `DEFAULT_CAPACITY`; `Some(c)` must match an existing arena exactly.
    pub fn open(name: &str, capacity: Option<u32>) -> Result<Arena> {
        check_pid()?; // first: before any mutex, fd or allocation
        validate_arena_name(name)?;
        if let Some(c) = capacity {
            if c == 0 || c > MAX_CAPACITY {
                return Err(Error::InvalidArgument(format!(
                    "capacity must be 1..={MAX_CAPACITY}"
                )));
            }
        }
        let check_cap = |inner: &ArenaInner| match capacity {
            Some(c) if c != inner.capacity => Err(Error::LayoutMismatch(format!(
                "arena '{name}' has capacity {}, requested {c}",
                inner.capacity
            ))),
            _ => Ok(()),
        };
        let cached = || arenas().lock().unwrap_or_else(|e| e.into_inner()).get(name).and_then(Weak::upgrade);
        if let Some(inner) = cached() {
            check_cap(&inner)?;
            return Ok(Arena { inner });
        }
        // Bootstrap WITHOUT holding the cache mutex: it may block on flock, and a
        // stuck lock on one arena must not wedge opens of every other arena. Two
        // threads racing here each get their own fd; that is still correct (flock
        // excludes different open file descriptions), and the loser adopts the
        // winner's mapping below.
        let fresh = Arc::new(Self::bootstrap(name, capacity)?);
        let mut cache = arenas().lock().unwrap_or_else(|e| e.into_inner());
        let inner = match cache.get(name).and_then(Weak::upgrade) {
            Some(existing) => existing,
            None => {
                cache.insert(name.to_string(), Arc::downgrade(&fresh));
                fresh
            }
        };
        drop(cache);
        check_cap(&inner)?;
        Ok(Arena { inner })
    }

    /// Open/create protocol. Runs once per process per arena (the cache above).
    fn bootstrap(name: &str, capacity: Option<u32>) -> Result<ArenaInner> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(shm_path(name))?;
        // /dev/shm is world-writable: refuse anything but a regular file we own,
        // before locking or resizing it (the arena name is a trust boundary).
        {
            use std::os::unix::fs::MetadataExt;
            let md = file.metadata()?;
            // SAFETY: geteuid is always safe.
            let me = unsafe { libc::geteuid() };
            if !md.file_type().is_file() || md.uid() != me {
                return Err(Error::LayoutMismatch(format!(
                    "arena '{name}': segment is not a regular file owned by uid {me}"
                )));
            }
        }
        let lock = Flock::exclusive(&file)?;

        // 1. Size. Only two states are recoverable, because this protocol only
        //    ever produces them: zero length (fresh) and a full-size segment whose
        //    magic is still 0 (a creator died before publishing; ftruncate is one
        //    call and magic is one 8-byte store). Anything else is not ours to
        //    rewrite, and is rejected without modification.
        let mut size = file.metadata()?.len();
        let want_new = arena_size(capacity.unwrap_or(DEFAULT_CAPACITY));
        if size == 0 {
            file.set_len(want_new)?;
            size = want_new;
            hooks::point("after_truncate");
        } else if size < SLOTS_OFFSET as u64 {
            return Err(Error::LayoutMismatch(format!(
                "arena '{name}': segment of {size} bytes is not an atomvar arena; refusing to modify it"
            )));
        }

        // 2. Map, then inspect the header only through the mapping.
        let mut map = map_len(&file, size)?;
        let hdr = |m: &MmapRaw| unsafe { &*(m.as_mut_ptr() as *const Header) };
        let magic = hdr(&map).magic.load(O::Acquire);
        if magic != 0 && magic != MAGIC {
            return Err(Error::LayoutMismatch(format!(
                "arena '{name}': unknown signature {:?}; refusing to modify it",
                String::from_utf8_lossy(&magic.to_le_bytes())
            )));
        }
        let cap = if magic == 0 {
            // Recovery is only for the exact state a crashed creator leaves: a
            // complete, correctly sized segment with zero signature, header
            // fields either zero or what a creator writes, and untouched (zero)
            // slots (nothing can be inserted before publication). Anything else
            // is not ours and is rejected without modification.
            if let Err(why) = unpublished_state_is_ours(&map, size) {
                return Err(Error::LayoutMismatch(format!(
                    "arena '{name}': zero signature but {why}; not an interrupted atomvar arena, refusing to modify it"
                )));
            }
            // Unpublished (fresh, or a creator crashed before publishing). Nobody
            // can be using it: publication requires this same lock.
            if size != want_new {
                // Clear the unpublished header fields BEFORE resizing, so a crash
                // between the two leaves "zero header + valid size", which the
                // validator accepts (fields may be zero), instead of a stored
                // capacity that disagrees with the new size.
                let h = hdr(&map);
                // SAFETY: exclusive flock held and magic is 0: nobody trusts these.
                unsafe {
                    *h.version.get() = 0;
                    *h.capacity.get() = 0;
                    *h.slot_size.get() = 0;
                    *h._pad0.get() = 0;
                    *h.layout_hash.get() = 0;
                }
                // The stores go to the shared page cache and must land before
                // the resize (tmpfs: a process crash cannot lose them).
                std::sync::atomic::fence(O::SeqCst);
                drop(map);
                file.set_len(want_new)?;
                hooks::point("after_recovery_resize");
                map = map_len(&file, want_new)?;
            }
            let h = hdr(&map);
            let cap = capacity.unwrap_or(DEFAULT_CAPACITY);
            // SAFETY: exclusive flock held and magic unset, so no reader trusts
            // these fields yet.
            unsafe {
                *h.version.get() = VERSION;
                *h.capacity.get() = cap;
                *h.slot_size.get() = SLOT_SIZE as u32;
                *h._pad0.get() = 0;
                *h.layout_hash.get() = LAYOUT_HASH;
                *h._reserved.get() = [0; 32];
            }
            hooks::point("before_magic");
            h.magic.store(MAGIC, O::Release);
            cap
        } else {
            let h = hdr(&map);
            // SAFETY: magic observed with Acquire; fields are immutable after it.
            let (ver, cap, ss, lh) = unsafe {
                (*h.version.get(), *h.capacity.get(), *h.slot_size.get(), *h.layout_hash.get())
            };
            if ver != VERSION || ss as usize != SLOT_SIZE || lh != LAYOUT_HASH {
                return Err(Error::LayoutMismatch(format!(
                    "arena '{name}': version {ver}, slot_size {ss}, layout {lh:#x}; this build expects \
                     version {VERSION}, slot_size {SLOT_SIZE}, layout {LAYOUT_HASH:#x}"
                )));
            }
            if cap == 0 || cap > MAX_CAPACITY || size != arena_size(cap) {
                return Err(Error::LayoutMismatch(format!(
                    "arena '{name}': header capacity {cap} does not match segment size {size}"
                )));
            }
            if let Some(c) = capacity {
                if c != cap {
                    return Err(Error::LayoutMismatch(format!(
                        "arena '{name}' has capacity {cap}, requested {c}"
                    )));
                }
            }
            cap
        };
        drop(lock);

        let base = NonNull::new(map.as_mut_ptr()).expect("mmap returned null");
        Ok(ArenaInner {
            name: name.to_string(),
            file,
            _map: map,
            base,
            capacity: cap,
            insert_lock: Mutex::new(()),
        })
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn capacity(&self) -> u32 {
        self.inner.capacity
    }

    /// Finds or (if `create`) creates `name` with type `ty`. The initial value is
    /// applied only when this call creates the variable.
    fn resolve(&self, name: &str, ty: ValueType, init: u128, create: bool) -> Result<usize> {
        check_pid()?;
        validate_var_name(name)?;
        let key = name.as_bytes();
        let found = |slot: usize, tag: u8| -> Result<usize> {
            let existing = ValueType::from_tag(tag).ok_or_else(|| {
                Error::LayoutMismatch(format!("slot {slot} has unknown type tag {tag}"))
            })?;
            if existing != ty {
                return Err(Error::TypeMismatch {
                    name: name.to_string(),
                    existing,
                    requested: ty,
                });
            }
            Ok(slot)
        };

        // Fast path: lock-free probe.
        match algo::probe(&*self.inner, key) {
            Probe::Found { slot, type_tag } => return found(slot, type_tag),
            _ if !create => {
                return Err(Error::NotFound {
                    name: name.to_string(),
                })
            }
            _ => {}
        }

        // Slow path: process mutex first (flock does not exclude threads sharing
        // this fd), then the cross-process flock. Poisoning is ignored: the shared
        // state is protected by the recovery protocol, not by the mutex's data.
        let _m = self
            .inner
            .insert_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _f = Flock::exclusive(&self.inner.file)?;
        // SAFETY: both insert locks are held.
        match unsafe { algo::insert_locked(&*self.inner, key, ty as u8, init) } {
            Inserted::Existing { slot, type_tag } => found(slot, type_tag),
            Inserted::Created { slot } => Ok(slot),
            Inserted::Full => Err(Error::ArenaFull {
                arena: self.inner.name.clone(),
                capacity: self.inner.capacity,
            }),
        }
    }

    fn owner(&self) -> Owner {
        Owner::Arena(self.inner.clone())
    }

    /// Opens or creates a variable of the given type. `init` is used only on creation.
    pub fn var(&self, name: &str, init: Value) -> Result<AnyAtomic> {
        let ty = init.value_type();
        let slot = self.resolve(name, ty, init.raw(), true)?;
        Ok(AnyAtomic::from_slot(ty, self.inner.value_ptr(slot), self.owner()))
    }

    /// Opens an existing variable; `NotFound` if it does not exist.
    pub fn open_var(&self, name: &str, ty: ValueType) -> Result<AnyAtomic> {
        let slot = self.resolve(name, ty, 0, false)?;
        Ok(AnyAtomic::from_slot(ty, self.inner.value_ptr(slot), self.owner()))
    }

    /// Looks a name up without knowing its type.
    pub fn lookup(&self, name: &str) -> Result<Option<VarInfo>> {
        check_pid()?;
        validate_var_name(name)?;
        Ok(match algo::probe(&*self.inner, name.as_bytes()) {
            Probe::Found { slot, type_tag } => Some(VarInfo {
                name: name.to_string(),
                value_type: ValueType::from_tag(type_tag).ok_or_else(|| {
                    Error::LayoutMismatch(format!("slot {slot} has unknown type tag {type_tag}"))
                })?,
                slot: slot as u32,
            }),
            _ => None,
        })
    }

    /// All published variables, in slot order.
    pub fn list(&self) -> Result<Vec<VarInfo>> {
        check_pid()?;
        let mut out = Vec::new();
        for i in 0..self.inner.capacity as usize {
            if self.inner.load_state(i, O::Acquire) != READY {
                continue;
            }
            // SAFETY: READY observed with Acquire.
            let (tag, name) = unsafe {
                let s = self.inner.slot(i);
                let len = *s.name_len.get() as usize;
                let stored: &[u8; MAX_VAR_NAME_LEN] = &*s.name.get();
                // A foreign/corrupt writer could store len > 64: skip such slots
                // (like unknown type tags) instead of panicking.
                (*s.type_tag.get(), stored.get(..len).map(<[u8]>::to_vec))
            };
            let Some(name) = name else { continue };
            if let Some(value_type) = ValueType::from_tag(tag) {
                out.push(VarInfo {
                    name: String::from_utf8_lossy(&name).into_owned(),
                    value_type,
                    slot: i as u32,
                });
            }
        }
        Ok(out)
    }

    /// Number of published variables.
    pub fn len(&self) -> Result<usize> {
        Ok(self.list()?.len())
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Test hook: holds both registry locks for `d` (used by the fork test).
    #[cfg(feature = "test-hooks")]
    pub fn test_hold_insert_locks(&self, d: std::time::Duration) -> Result<()> {
        let _m = self.inner.insert_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _f = Flock::exclusive(&self.inner.file)?;
        std::thread::sleep(d);
        Ok(())
    }
}

macro_rules! typed_open {
    ($($create:ident, $open:ident, $variant:ident, $rust:ty, $handle:ident;)*) => {
        impl Arena {
            $(
                #[doc = concat!("Opens or creates a `", stringify!($rust), "` variable; `init` is used only on creation.")]
                pub fn $create(&self, name: &str, init: $rust) -> Result<crate::handles::$handle> {
                    match self.var(name, Value::$variant(init))? {
                        AnyAtomic::$variant(h) => Ok(h),
                        _ => unreachable!(),
                    }
                }

                #[doc = concat!("Opens an existing `", stringify!($rust), "` variable.")]
                pub fn $open(&self, name: &str) -> Result<crate::handles::$handle> {
                    match self.open_var(name, ValueType::$variant)? {
                        AnyAtomic::$variant(h) => Ok(h),
                        _ => unreachable!(),
                    }
                }
            )*
        }
    };
}

typed_open! {
    i64, open_i64, I64, i64, AtomicI64;
    u64, open_u64, U64, u64, AtomicU64;
    bool, open_bool, Bool, bool, AtomicBool;
    f64, open_f64, F64, f64, AtomicF64;
    u128, open_u128, U128, u128, AtomicU128;
}
