//! Crash hooks for the failure tests. Compiled to nothing without `test-hooks`.
//!
//! With the feature on, setting `ATOMVAR_TEST_CRASH_AT=<point>` makes the process
//! SIGKILL itself when it reaches that point, simulating a crash mid-protocol.

#[cfg(feature = "test-hooks")]
pub(crate) fn point(name: &str) {
    if std::env::var("ATOMVAR_TEST_CRASH_AT").as_deref() == Ok(name) {
        // SAFETY: plain syscalls; the process dies immediately.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
    }
}

#[cfg(not(feature = "test-hooks"))]
#[inline(always)]
pub(crate) fn point(_name: &str) {}
