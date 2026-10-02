//! Compiles tests/c/ffi_test.c against include/atomvar.h with -Werror, links it
//! to the freshly built libatomvar.so, and runs it. Fails (does not skip) if gcc
//! is missing.

use std::path::PathBuf;
use std::process::Command;

/// Directory holding libatomvar.so: `cargo test` leaves the cdylib next to the
/// test binary in <target>/<profile>/deps; `cargo build` uplifts it one level.
fn lib_dir() -> PathBuf {
    let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let profile = deps.parent().unwrap().to_path_buf();
    [deps, profile]
        .into_iter()
        .find(|d| d.join("libatomvar.so").exists())
        .expect("libatomvar.so not found next to the test binary")
}

#[test]
fn c_program_exercises_every_function() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_dir = lib_dir();
    let out = std::env::temp_dir().join(format!("atomvar_ffi_test_{}", std::process::id()));
    let status = Command::new("gcc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg("-I")
        .arg(crate_dir.join("include"))
        .arg(crate_dir.join("tests/c/ffi_test.c"))
        .arg("-L")
        .arg(&lib_dir)
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .args(["-latomvar", "-lm", "-o"])
        .arg(&out)
        .output()
        .expect("gcc is required for this test");
    assert!(
        status.status.success(),
        "gcc failed:\n{}",
        String::from_utf8_lossy(&status.stderr)
    );

    let arena = format!("test-ffi-{}", std::process::id());
    let _ = std::fs::remove_file(format!("/dev/shm/atomvar-{arena}"));
    let o = Command::new(&out).arg(&arena).output().unwrap();
    let _ = std::fs::remove_file(format!("/dev/shm/atomvar-{arena}"));
    let _ = std::fs::remove_file(&out);
    assert!(
        o.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "ALL OK");
}
