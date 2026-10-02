#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};

/// Unique arena name per test; the segment is removed on drop.
pub struct TempArena(pub String);

impl TempArena {
    pub fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let name = format!("test-{tag}-{}-{n}", std::process::id());
        let _ = std::fs::remove_file(Self::path_of(&name));
        TempArena(name)
    }

    pub fn path_of(name: &str) -> String {
        format!("/dev/shm/atomvar-{name}")
    }

    pub fn path(&self) -> String {
        Self::path_of(&self.0)
    }
}

impl Drop for TempArena {
    fn drop(&mut self) {
        let _ = atomvar_core::destroy_arena(&self.0);
    }
}

/// Names (with a given prefix) that all hash to the same home slot.
pub fn colliding_names(capacity: u32, count: usize) -> Vec<String> {
    let home = |s: &str| atomvar_core::algo::fnv1a(s.as_bytes()) % capacity as u64;
    let target = home("collide-0");
    let mut out = vec!["collide-0".to_string()];
    let mut i = 1;
    while out.len() < count {
        let s = format!("collide-{i}");
        if home(&s) == target {
            out.push(s);
        }
        i += 1;
    }
    out
}
