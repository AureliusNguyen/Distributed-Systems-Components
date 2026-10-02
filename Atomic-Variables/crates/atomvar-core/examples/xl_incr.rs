//! Rust worker for the cross-language benchmark (benches/suite/crosslang).
//!   cargo run --release -p atomvar-core --example xl_incr -- <arena> <var> <n> <go-file>
//! Prints "ready", waits for <go-file>, does n x fetch_add(1), prints a JSON line.

use atomvar_core::{Arena, Ordering};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let c = Arena::open(&a[1], None).unwrap().i64(&a[2], 0).unwrap();
    let n: u64 = a[3].parse().unwrap();
    println!("ready");
    while !std::path::Path::new(&a[4]).exists() {
        std::thread::yield_now();
    }
    let t0 = Instant::now();
    for _ in 0..n {
        c.fetch_add(1, Ordering::SeqCst);
    }
    let el = t0.elapsed().as_nanos();
    println!("{{\"lang\": \"Rust\", \"ops\": {n}, \"elapsed_ns\": {el}}}");
}
