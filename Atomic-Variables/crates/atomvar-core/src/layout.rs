//! Shared-memory layout. Every offset here is part of the cross-process and
//! cross-language contract; the const assertions fail the build if it drifts.

use crate::algo::fnv1a;
use std::cell::UnsafeCell;
use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::{AtomicU32, AtomicU64};

pub const MAGIC: u64 = u64::from_le_bytes(*b"ATOMVAR1");
pub const VERSION: u32 = 1;
pub const SLOT_SIZE: usize = 128;
/// Slots start here so every slot (and its 16-byte value) is correctly aligned.
pub const SLOTS_OFFSET: usize = 128;
pub const MAX_VAR_NAME_LEN: usize = 64;
pub const MAX_ARENA_NAME_LEN: usize = 100;

const LAYOUT_DESC: &[u8] = b"atomvar;v1;header=64@0;slots@128;slot=128;state@0:u32;type@4:u8;\
name_len@5:u8;value@16:16;name@32:64;tags=i64:1,u64:2,bool:3,f64:4,u128:5;hash=fnv1a64";
pub const LAYOUT_HASH: u64 = fnv1a(LAYOUT_DESC);

#[repr(C, align(64))]
pub struct Header {
    /// Written LAST (Release) during bootstrap, read with Acquire.
    pub magic: AtomicU64,
    pub version: UnsafeCell<u32>,
    pub capacity: UnsafeCell<u32>,
    pub slot_size: UnsafeCell<u32>,
    pub _pad0: UnsafeCell<u32>,
    pub layout_hash: UnsafeCell<u64>,
    pub _reserved: UnsafeCell<[u8; 32]>,
}

/// 16-byte, 16-aligned value cell. Viewed as exactly one atomic type for the
/// slot's whole lifetime (AtomicI64/U64/Bool/U64-as-f64-bits/U128 at offset 0).
#[repr(C, align(16))]
pub struct ValueCell(pub UnsafeCell<[u8; 16]>);

// SAFETY: after initialization (before the cell is shared), the bytes are only
// ever accessed through atomic operations of the one type fixed for the cell.
unsafe impl Sync for ValueCell {}

#[repr(C, align(128))]
pub struct Slot {
    pub state: AtomicU32,
    pub type_tag: UnsafeCell<u8>,
    pub name_len: UnsafeCell<u8>,
    pub _pad0: UnsafeCell<[u8; 10]>,
    pub value: ValueCell,
    pub name: UnsafeCell<[u8; MAX_VAR_NAME_LEN]>,
    pub _reserved: UnsafeCell<[u8; 32]>,
}

const _: () = {
    assert!(size_of::<Header>() == 64);
    assert!(align_of::<Header>() == 64);
    assert!(offset_of!(Header, magic) == 0);
    assert!(offset_of!(Header, version) == 8);
    assert!(offset_of!(Header, capacity) == 12);
    assert!(offset_of!(Header, slot_size) == 16);
    assert!(offset_of!(Header, _pad0) == 20);
    assert!(offset_of!(Header, layout_hash) == 24);
    assert!(offset_of!(Header, _reserved) == 32);

    assert!(size_of::<ValueCell>() == 16);
    assert!(align_of::<ValueCell>() == 16);

    assert!(size_of::<Slot>() == SLOT_SIZE);
    assert!(align_of::<Slot>() == 128);
    assert!(offset_of!(Slot, state) == 0);
    assert!(offset_of!(Slot, type_tag) == 4);
    assert!(offset_of!(Slot, name_len) == 5);
    assert!(offset_of!(Slot, _pad0) == 6);
    assert!(offset_of!(Slot, value) == 16);
    assert!(offset_of!(Slot, name) == 32);
    assert!(offset_of!(Slot, _reserved) == 96);

    assert!(SLOTS_OFFSET >= size_of::<Header>());
    assert!(SLOTS_OFFSET % align_of::<Slot>() == 0);
    assert!(MAX_VAR_NAME_LEN <= u8::MAX as usize);
};

pub const fn arena_size(capacity: u32) -> u64 {
    SLOTS_OFFSET as u64 + capacity as u64 * SLOT_SIZE as u64
}
