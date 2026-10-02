//! gRPC worker for the cross-language benchmark: the same increments through
//! atomvard's gRPC API (tonic client; the fastest client, favoring gRPC).
//!   grpc_incr <port> <arena> <var> <n> <go-file>

use atomvard::grpc::pb;
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut cl = pb::atomic_service_client::AtomicServiceClient::connect(format!("http://127.0.0.1:{}", a[1]))
        .await
        .unwrap();
    let n: u64 = a[4].parse().unwrap();
    let req = || pb::FetchRequest {
        var: Some(pb::VarRef { arena: a[2].clone(), name: a[3].clone() }),
        op: pb::FetchOp::Add as i32,
        operand: Some(pb::Value { kind: Some(pb::value::Kind::I64(1)) }),
        ordering: 0,
        request_id: String::new(),
    };
    println!("ready");
    while !std::path::Path::new(&a[5]).exists() {
        tokio::task::yield_now().await;
    }
    let t0 = Instant::now();
    for _ in 0..n {
        cl.fetch(req()).await.unwrap();
    }
    let el = t0.elapsed().as_nanos();
    println!("{{\"lang\": \"Rust gRPC client\", \"ops\": {n}, \"elapsed_ns\": {el}}}");
}
