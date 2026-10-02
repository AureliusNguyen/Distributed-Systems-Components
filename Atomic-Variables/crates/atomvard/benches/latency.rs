//! `cargo bench -p atomvard`: per-op cost of fetch_add through each access path.
//! Plain std timing (no criterion): the paths differ by orders of magnitude.

use atomvar_core::{Arena, AtomicI64, Ordering};
use atomvard::grpc::pb;
use atomvard::{http_app, serve_grpc, Service};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn native(label: &str, h: &AtomicI64, threads: usize, per_thread: u64) {
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                for _ in 0..per_thread {
                    h.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    let ns = t0.elapsed().as_nanos() as f64 / (threads as u64 * per_thread) as f64;
    println!("{label:<44} {ns:>10.1} ns/op");
}

fn main() {
    let arena_name = format!("bench-{}", std::process::id());
    let arena = Arena::open(&arena_name, Some(16)).unwrap();

    println!("{:<44} {:>16}", "path", "cost");
    let heap = AtomicI64::new(0);
    native("native heap fetch_add, 1 thread", &heap, 1, 20_000_000);
    native("native heap fetch_add, 2 threads contended", &heap, 2, 10_000_000);
    let shm = arena.i64("c", 0).unwrap();
    native("native shm fetch_add, 1 thread", &shm, 1, 20_000_000);
    native("native shm fetch_add, 2 threads contended", &shm, 2, 10_000_000);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let svc = Service::new(10_000, Duration::from_secs(600));

        // HTTP/JSON, sequential requests over one keep-alive connection.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let app = http_app(svc.clone(), None, false);
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/arenas/{arena_name}/vars/c/fetch_add");
        let n = 5_000;
        for _ in 0..200 {
            client.post(&url).json(&serde_json::json!({"value": 1})).send().await.unwrap();
        }
        let t0 = Instant::now();
        for _ in 0..n {
            let r = client.post(&url).json(&serde_json::json!({"value": 1})).send().await.unwrap();
            assert!(r.status().is_success());
            r.bytes().await.unwrap();
        }
        let us = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!("{:<44} {us:>10.1} microsec/op", "HTTP/JSON fetch_add (sequential)");

        // gRPC, sequential unary calls.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let gaddr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        tokio::spawn(serve_grpc(svc.clone(), gaddr, None));
        let mut cl = loop {
            match pb::atomic_service_client::AtomicServiceClient::connect(format!("http://{gaddr}")).await {
                Ok(c) => break c,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        let req = || pb::FetchRequest {
            var: Some(pb::VarRef { arena: arena_name.clone(), name: "c".into() }),
            op: pb::FetchOp::Add as i32,
            operand: Some(pb::Value { kind: Some(pb::value::Kind::I64(1)) }),
            ordering: 0,
            request_id: String::new(),
        };
        for _ in 0..200 {
            cl.fetch(req()).await.unwrap();
        }
        let t0 = Instant::now();
        for _ in 0..n {
            cl.fetch(req()).await.unwrap();
        }
        let us = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!("{:<44} {us:>10.1} microsec/op", "gRPC fetch_add (sequential)");

        // With a request_id (idempotency table + detached task).
        let t0 = Instant::now();
        for i in 0..n {
            let mut r = req();
            r.request_id = format!("bench-{i}");
            cl.fetch(r).await.unwrap();
        }
        let us = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!("{:<44} {us:>10.1} microsec/op", "gRPC fetch_add with request_id");
        let _: Arc<Service> = svc;
    });

    drop(shm);
    drop(arena);
    let _ = atomvar_core::destroy_arena(&arena_name);
}
