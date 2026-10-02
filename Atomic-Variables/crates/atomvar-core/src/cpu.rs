use crate::error::{Error, Result};

/// Raw CPUID check for CMPXCHG16B (leaf 1, ECX bit 13).
///
/// This is a best-effort diagnostic, NOT a guarantee: the whole build is compiled
/// with `+cmpxchg16b`, so the compiler may already have used the instruction
/// before this runs. `is_x86_feature_detected!` would be constant-folded to true
/// under that flag, which is why CPUID is queried directly.
pub fn cpu_supported() -> bool {
    // SAFETY: CPUID leaf 1 exists on every x86_64 CPU (safe fn on newer Rust).
    #[allow(unused_unsafe)]
    let r = unsafe { core::arch::x86_64::__cpuid(1) };
    r.ecx & (1 << 13) != 0
}

/// Optional library init: reports an unsupported CPU if it gets the chance.
pub fn init() -> Result<()> {
    if cpu_supported() {
        Ok(())
    } else {
        Err(Error::UnsupportedCpu)
    }
}
