//! Child-process helper for the cross-process failure tests (tests/process.rs).
//! Built only with the `test-hooks` feature.
//!
//! Usage:
//!   atomvar-testhelper incr  <arena> <var> <n>           fetch_add(1) n times
//!   atomvar-testhelper claim <arena> <var> <token>        CAS 0 -> token, prints "won"/"lost"
//!   atomvar-testhelper create <arena> <var> [<var>...]    create each var (i64, init 0)
//!   atomvar-testhelper open  <arena> [capacity]           open (and maybe create) the arena
//!   atomvar-testhelper wait-then-incr <arena> <var> <n> <go-file>
//!                                                         spin until go-file exists, then incr

use atomvar_core::{Arena, Ordering};
use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let res = run(&args[1..]);
    if let Err(e) = res {
        eprintln!("helper error: {e}");
        exit(2);
    }
}

fn wait_for(path: &str) {
    while !std::path::Path::new(path).exists() {
        std::thread::yield_now();
    }
}

fn run(a: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match a.first().map(String::as_str) {
        Some("incr") => {
            let c = Arena::open(&a[1], None)?.i64(&a[2], 0)?;
            let n: u64 = a[3].parse()?;
            for _ in 0..n {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }
        Some("wait-then-incr") => {
            let c = Arena::open(&a[1], None)?.i64(&a[2], 0)?;
            let n: u64 = a[3].parse()?;
            wait_for(&a[4]);
            for _ in 0..n {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }
        Some("claim") => {
            let c = Arena::open(&a[1], None)?.u64(&a[2], 0)?;
            let token: u64 = a[3].parse()?;
            if let Some(go) = a.get(4) {
                wait_for(go);
            }
            match c.compare_exchange(0, token, Ordering::SeqCst, Ordering::SeqCst)? {
                Ok(_) => println!("won"),
                Err(_) => println!("lost"),
            }
        }
        Some("create") => {
            let arena = Arena::open(&a[1], None)?;
            let go = std::env::var("ATOMVAR_TEST_GO_FILE").ok();
            if let Some(go) = go {
                wait_for(&go);
            }
            for name in &a[2..] {
                let c = arena.i64(name, 0)?;
                c.fetch_add(1, Ordering::SeqCst);
            }
        }
        Some("open") => {
            let cap = a.get(2).map(|s| s.parse()).transpose()?;
            if let Ok(go) = std::env::var("ATOMVAR_TEST_GO_FILE") {
                wait_for(&go);
            }
            let arena = Arena::open(&a[1], cap)?;
            if std::env::var("ATOMVAR_TEST_CREATE_VAR").is_ok() {
                arena.i64("v", 7)?;
            }
            println!("capacity={}", arena.capacity());
        }
        _ => return Err("unknown command".into()),
    }
    Ok(())
}
