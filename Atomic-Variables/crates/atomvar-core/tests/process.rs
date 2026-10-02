//! Cross-process behavior using real child processes (fork+exec of the helper
//! binary), including crash-recovery and fork-detection failure cases.

mod common;

use atomvar_core::*;
use common::TempArena;
use std::collections::HashSet;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const SC: Ordering = Ordering::SeqCst;
const HELPER: &str = env!("CARGO_BIN_EXE_atomvar-testhelper");

fn helper(args: &[&str]) -> Command {
    let mut c = Command::new(HELPER);
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

fn run(c: &mut Command) -> Output {
    c.output().expect("spawn helper")
}

fn wait_all(children: Vec<Child>) -> Vec<Output> {
    children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect()
}

fn go_file(tag: &str) -> String {
    let p = format!(
        "{}/atomvar-go-{tag}-{}",
        std::env::temp_dir().display(),
        std::process::id()
    );
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn exact_total_across_processes() {
    let t = TempArena::new("total");
    Arena::open(&t.0, Some(16)).unwrap(); // create it up front
    let go = go_file("total");
    let (procs, n) = (8, 1_000_000u64);
    let children: Vec<_> = (0..procs)
        .map(|_| {
            helper(&["wait-then-incr", &t.0, "counter", &n.to_string(), &go])
                .spawn()
                .unwrap()
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(&go, b"go").unwrap();
    for o in wait_all(children) {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let _ = std::fs::remove_file(&go);
    let c = Arena::open(&t.0, None).unwrap().open_i64("counter").unwrap();
    assert_eq!(c.load(SC).unwrap(), (procs as i64) * n as i64);
}

#[test]
fn claim_once_has_exactly_one_winner() {
    let t = TempArena::new("claim");
    let arena = Arena::open(&t.0, Some(16)).unwrap();
    let flag = arena.u64("leader", 0).unwrap();
    let go = go_file("claim");
    let children: Vec<_> = (1..=8)
        .map(|tok| helper(&["claim", &t.0, "leader", &tok.to_string(), &go]).spawn().unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(&go, b"go").unwrap();
    let outs = wait_all(children);
    let _ = std::fs::remove_file(&go);
    let winners: Vec<usize> = outs
        .iter()
        .enumerate()
        .filter(|(_, o)| String::from_utf8_lossy(&o.stdout).trim() == "won")
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(winners.len(), 1, "winners: {winners:?}");
    assert_eq!(flag.load(SC).unwrap(), winners[0] as u64);
}

#[test]
fn concurrent_same_name_creation_across_processes() {
    let t = TempArena::new("samename");
    let arena = Arena::open(&t.0, Some(32)).unwrap();
    let go = go_file("samename");
    let children: Vec<_> = (0..16)
        .map(|_| {
            helper(&["create", &t.0, "x", "y", "z"])
                .env("ATOMVAR_TEST_GO_FILE", &go)
                .spawn()
                .unwrap()
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(&go, b"go").unwrap();
    for o in wait_all(children) {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let _ = std::fs::remove_file(&go);
    let vars = arena.list().unwrap();
    let names: HashSet<_> = vars.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(vars.len(), 3, "{vars:?}");
    assert_eq!(names, HashSet::from(["x", "y", "z"]));
    for n in ["x", "y", "z"] {
        assert_eq!(arena.open_i64(n).unwrap().load(SC).unwrap(), 16);
    }
}

#[test]
fn concurrent_first_bootstrap_of_a_fresh_arena() {
    // No pre-creation: 16 processes race to create the same fresh arena.
    for round in 0..3 {
        let t = TempArena::new(&format!("fresh{round}"));
        let go = go_file(&format!("fresh{round}"));
        let children: Vec<_> = (0..16)
            .map(|_| {
                helper(&["open", &t.0, "16"])
                    .env("ATOMVAR_TEST_GO_FILE", &go)
                    .env("ATOMVAR_TEST_CREATE_VAR", "1")
                    .spawn()
                    .unwrap()
            })
            .collect();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&go, b"go").unwrap();
        for o in wait_all(children) {
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "capacity=16");
        }
        let _ = std::fs::remove_file(&go);
        assert_eq!(std::fs::metadata(t.path()).unwrap().len(), 128 + 16 * 128);
        let arena = Arena::open(&t.0, None).unwrap();
        assert_eq!(arena.list().unwrap().len(), 1, "one 'v' variable, created once");
        assert_eq!(arena.open_i64("v").unwrap().load(SC).unwrap(), 7);
    }
}

#[test]
fn refuses_symlinked_or_foreign_segments() {
    let t = TempArena::new("symlink");
    let target = std::env::temp_dir().join(format!("atomvar-victim-{}", std::process::id()));
    std::fs::write(&target, b"do not truncate me").unwrap();
    std::os::unix::fs::symlink(&target, t.path()).unwrap();
    assert!(Arena::open(&t.0, Some(8)).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"do not truncate me");
    std::fs::remove_file(t.path()).unwrap();
    std::fs::remove_file(&target).unwrap();
}

#[test]
fn corrupt_name_len_is_skipped_not_a_panic() {
    use std::os::unix::fs::FileExt;
    let t = TempArena::new("corrupt");
    // create in a child so this process maps it fresh afterwards
    let o = run(helper(&["open", &t.0, "8"]).env("ATOMVAR_TEST_CREATE_VAR", "1"));
    assert!(o.status.success());
    let slot = Arena::open(&t.0, None).unwrap().lookup("v").unwrap().unwrap().slot as u64;
    let f = std::fs::OpenOptions::new().write(true).open(t.path()).unwrap();
    f.write_all_at(&[200u8], 128 + slot * 128 + 5).unwrap(); // name_len
    let arena = Arena::open(&t.0, None).unwrap();
    assert!(arena.list().unwrap().is_empty());
}

fn assert_killed(o: &Output) {
    assert_eq!(
        o.status.signal(),
        Some(libc::SIGKILL),
        "expected SIGKILL, got {:?} stderr={}",
        o.status,
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn creator_killed_before_magic_is_recovered() {
    for point in ["after_truncate", "before_magic"] {
        let t = TempArena::new(point);
        let o = run(helper(&["open", &t.0, "16"]).env("ATOMVAR_TEST_CRASH_AT", point));
        assert_killed(&o);
        assert!(std::path::Path::new(&t.path()).exists());

        // Next opener (different capacity, even) takes over the unpublished segment.
        // It must not block: the killed process's flock died with it.
        let o = run(&mut helper(&["open", &t.0, "32"]));
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "capacity=32");

        let arena = Arena::open(&t.0, None).unwrap();
        assert_eq!(arena.capacity(), 32);
        assert_eq!(arena.i64("ok", 1).unwrap().increment_and_get(SC), 2);
    }
}

/// A crash DURING recovery (after it resized an unpublished segment) must
/// itself be recoverable, whatever capacity the next opener asks for.
#[test]
fn recovery_killed_after_resize_is_recovered() {
    for final_cap in ["16", "32", "8"] {
        let t = TempArena::new(&format!("rec{final_cap}"));
        // 1. creator (capacity 16) dies before publishing
        let o = run(helper(&["open", &t.0, "16"]).env("ATOMVAR_TEST_CRASH_AT", "before_magic"));
        assert_killed(&o);
        // 2. recovery with capacity 32 dies right after resizing
        let o = run(helper(&["open", &t.0, "32"]).env("ATOMVAR_TEST_CRASH_AT", "after_recovery_resize"));
        assert_killed(&o);
        assert_eq!(std::fs::metadata(t.path()).unwrap().len(), 128 + 32 * 128, "resize happened");
        // 3. the next opener succeeds, with any capacity
        let o = run(helper(&["open", &t.0, final_cap]).env("ATOMVAR_TEST_CREATE_VAR", "1"));
        assert!(o.status.success(), "cap {final_cap}: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), format!("capacity={final_cap}"));
        let arena = Arena::open(&t.0, None).unwrap();
        assert_eq!(arena.capacity().to_string(), final_cap);
        assert_eq!(arena.open_i64("v").unwrap().load(SC).unwrap(), 7);
    }
}

#[test]
fn inserter_killed_mid_insert_is_recovered() {
    let t = TempArena::new("midinsert");
    let arena = Arena::open(&t.0, Some(8)).unwrap();
    let o = run(helper(&["create", &t.0, "victim"])
        .env("ATOMVAR_TEST_CRASH_AT", "after_payload_before_ready"));
    assert_killed(&o);
    // Debris: payload written, never published.
    assert!(arena.lookup("victim").unwrap().is_none());
    assert!(arena.list().unwrap().is_empty());

    // A later inserter (different type, even) reclaims the slot; no duplicates.
    let v = arena.bool("victim", true).unwrap();
    assert!(v.load(SC).unwrap());
    let vars = arena.list().unwrap();
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].value_type, ValueType::Bool);
    // and other processes see exactly that one
    let o = run(&mut helper(&["create", &t.0, "other"]));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(arena.list().unwrap().len(), 2);
}

#[test]
fn layout_mismatch_on_reopen_is_an_error() {
    // capacity mismatch, seen by a fresh process
    let t = TempArena::new("layoutx");
    let o = run(helper(&["open", &t.0, "8"]).env("ATOMVAR_TEST_CREATE_VAR", "1"));
    assert!(o.status.success());
    let o = run(&mut helper(&["open", &t.0, "16"]));
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("layout mismatch"));

    // corrupted layout hash (offset 24)
    use std::os::unix::fs::FileExt;
    let f = std::fs::OpenOptions::new().write(true).open(t.path()).unwrap();
    f.write_all_at(&0xdead_beef_u64.to_le_bytes(), 24).unwrap();
    let o = run(&mut helper(&["open", &t.0]));
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("layout mismatch"));
}

/// Plain fork() (no exec) is unsupported: the child must get ForkedProcess
/// promptly, even when another parent thread held both registry locks at fork
/// time. A hang fails the test via the timeout.
#[test]
fn forked_child_is_refused_not_hung() {
    let t = TempArena::new("fork");
    let arena = Arena::open(&t.0, Some(8)).unwrap();
    arena.i64("pre", 0).unwrap();

    let holder = {
        let arena = arena.clone();
        std::thread::spawn(move || arena.test_hold_insert_locks(Duration::from_secs(2)).unwrap())
    };
    std::thread::sleep(Duration::from_millis(300)); // holder now owns mutex + flock

    let name: &str = &t.0; // allocated before fork
    // SAFETY: the child only calls getpid-guarded paths that return before
    // touching locks or allocating, then _exit.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let a = matches!(Arena::open(name, None), Err(Error::ForkedProcess));
        let b = matches!(arena.i64("new-in-child", 0), Err(Error::ForkedProcess));
        let c = matches!(arena.lookup("pre"), Err(Error::ForkedProcess));
        unsafe { libc::_exit(if a && b && c { 0 } else { 1 }) };
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut status = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            break;
        }
        if Instant::now() > deadline {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            unsafe { libc::waitpid(pid, &mut status, 0) };
            panic!("forked child hung");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(libc::WIFEXITED(status), "child status {status}");
    assert_eq!(libc::WEXITSTATUS(status), 0, "child did not get ForkedProcess everywhere");
    holder.join().unwrap();
    // parent still fine
    assert!(arena.i64("after", 0).is_ok());
}

#[test]
fn unknown_signature_or_foreign_file_is_rejected_unmodified() {
    let big: Vec<u8> = b"ATOMVAR2"
        .iter()
        .copied()
        .chain(std::iter::repeat(0u8).take(120))
        .chain(b"old-format-data!".iter().copied())
        .chain(std::iter::repeat(0u8).take(113))
        .collect();
    let small = b"not an arena".to_vec();
    // zero signature, but 257 bytes cannot be a complete arena of this layout
    let zeros_257 = vec![0u8; 257];
    // zero signature and a valid size (capacity 8), but data in the slot area:
    // no creator of this protocol can leave that behind
    let mut stray = vec![0u8; 128 + 8 * 128];
    stray[600] = 0x5a;
    // zero signature, valid size, header field this layout never writes
    let mut bad_hdr = vec![0u8; 128 + 8 * 128];
    bad_hdr[16..20].copy_from_slice(&64u32.to_le_bytes()); // slot_size 64
    for (tag, content) in [
        ("magic", big),
        ("small", small),
        ("zeros257", zeros_257),
        ("stray", stray),
        ("badhdr", bad_hdr),
    ] {
        let t = TempArena::new(tag);
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(t.path())
                .unwrap();
            std::io::Write::write_all(&mut f, &content).unwrap();
        }
        match Arena::open(&t.0, Some(8)) {
            Err(Error::LayoutMismatch(msg)) => assert!(msg.contains("refusing to modify"), "{msg}"),
            other => panic!("{tag}: expected LayoutMismatch, got {other:?}"),
        }
        assert_eq!(std::fs::read(t.path()).unwrap(), content, "{tag}: file was modified");
    }
}
