//! End-to-end: real HTTP and gRPC servers, a real MCP stdio session with the
//! atomvard binary, interleaved with native increments on the same variable.

use atomvar_core::{Arena, Ordering};
use atomvard::grpc::pb;
use atomvard::{http_app, serve_grpc, Service};
use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

struct TempArena(String);
impl TempArena {
    fn new(tag: &str) -> Self {
        TempArena(format!("test-t-{tag}-{}", std::process::id()))
    }
}
impl Drop for TempArena {
    fn drop(&mut self) {
        let _ = atomvar_core::destroy_arena(&self.0);
    }
}

fn svc() -> Arc<Service> {
    Service::new(1000, Duration::from_secs(600))
}

async fn start_http(token: Option<&str>) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let app = http_app(svc(), token.map(Into::into), true);
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_interleaved_with_native_is_exact_and_wire_is_exact() {
    let t = TempArena::new("http");
    let base = start_http(Some("s3cret")).await;
    let c = reqwest::Client::new();
    let url = |p: &str| format!("{base}/v1/arenas/{}/vars/{p}", t.0);
    let auth = "Bearer s3cret";

    // auth
    let r = c.get(format!("{base}/v1/health")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = c.get(format!("{base}/v1/health")).header("authorization", "Bearer nope").send().await.unwrap();
    assert_eq!(r.status(), 401);

    let client = c.clone();
    let post = move |p: String, body: J| {
        let c = client.clone();
        async move {
            let r = c.post(p).header("authorization", auth).json(&body).send().await.unwrap();
            let status = r.status().as_u16();
            (status, r.json::<J>().await.unwrap())
        }
    };

    let (s, b) = post(url("counter/create"), json!({"type": "i64", "capacity": 32})).await;
    assert_eq!((s, &b["value"]), (200, &json!("0")), "{b}");

    // HTTP increments interleaved with native increments on the same word
    let native = {
        let name = t.0.clone();
        std::thread::spawn(move || {
            let h = Arena::open(&name, None).unwrap().open_i64("counter").unwrap();
            for _ in 0..20_000 {
                h.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let post = post.clone();
        let u = url("counter/fetch_add");
        tasks.push(tokio::spawn(async move {
            for _ in 0..250 {
                let (s, _) = post(u.clone(), json!({"value": 1})).await;
                assert_eq!(s, 200);
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    native.join().unwrap();
    let r: J = c.get(url("counter")).header("authorization", auth).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["value"], json!("21000"));

    // exact wire round trips through set + get
    let cases = [
        ("i", "i64", json!("-9223372036854775808")),
        ("u", "u64", json!("18446744073709551615")),
        ("w", "u128", json!("340282366920938463463374607431768211455")),
        ("b", "bool", json!(true)),
    ];
    for (name, ty, v) in cases {
        post(url(&format!("{name}/create")), json!({"type": ty})).await;
        let (s, b) = post(url(&format!("{name}/set")), json!({"value": v})).await;
        assert_eq!(s, 200, "{b}");
        let got: J = c.get(url(name)).header("authorization", auth).send().await.unwrap().json().await.unwrap();
        assert_eq!(got["value"], v, "{name}");
    }
    post(url("f/create"), json!({"type": "f64"})).await;
    for bits in ["0x7ff8000000000001", "0xfff8deadbeef0001", "0x8000000000000000"] {
        post(url("f/set"), json!({"value": {"bits": bits}})).await;
        let got: J = c.get(url("f")).header("authorization", auth).send().await.unwrap().json().await.unwrap();
        assert_eq!(got["value"]["bits"], json!(bits));
    }
    let got: J = c.get(url("f")).header("authorization", auth).send().await.unwrap().json().await.unwrap();
    assert_eq!(got["value"]["value"], json!("-0.0"));

    // CAS + fetch shapes
    let (_, b) = post(url("counter/compare_exchange"), json!({"expected": "21000", "desired": 5})).await;
    assert_eq!(b, json!({"exchanged": true, "previous": "21000"}));
    let (_, b) = post(url("counter/fetch_max"), json!({"value": 9})).await;
    assert_eq!(b, json!({"previous": "5", "current": "9"}));

    // Idempotency-Key header
    let r1 = c.post(url("counter/fetch_add")).header("authorization", auth).header("idempotency-key", "k1").json(&json!({"value": 1})).send().await.unwrap().json::<J>().await.unwrap();
    let r2 = c.post(url("counter/fetch_add")).header("authorization", auth).header("idempotency-key", "k1").json(&json!({"value": 1})).send().await.unwrap().json::<J>().await.unwrap();
    assert_eq!(r1, r2);
    assert_eq!(r1["current"], json!("10"));

    // error mapping
    let (s, b) = post(url("counter/create"), json!({"type": "u64"})).await;
    assert_eq!((s, b["error"]["code"].as_str()), (409, Some("TYPE_MISMATCH")));
    let (s, b) = post(url("missing/get"), json!({})).await;
    assert_eq!((s, b["error"]["code"].as_str()), (404, Some("NOT_FOUND")));
    let (s, b) = post(url("counter/get"), json!({"ordering": "release"})).await;
    assert_eq!((s, b["error"]["code"].as_str()), (400, Some("INVALID_ORDERING")));
    let (s, b) = post(url("w/fetch_add"), json!({"value": 1})).await;
    assert_eq!((s, b["error"]["code"].as_str()), (400, Some("INVALID_ARGUMENT")));
    let (s, b) = post(url("counter/set"), json!({"value": 1.5})).await;
    assert_eq!((s, b["error"]["code"].as_str()), (400, Some("INVALID_ARGUMENT")));
    let (s, b) = post(url("counter/set"), json!({"valu": 1})).await;
    assert_eq!((s, b["error"]["code"].as_str()), (400, Some("INVALID_ARGUMENT")), "unknown fields rejected");

    let r: J = c.get(format!("{base}/v1/arenas/{}/vars", t.0)).header("authorization", auth).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["variables"].as_array().unwrap().len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_round_trip_with_auth() {
    use pb::atomic_service_client::AtomicServiceClient;
    let t = TempArena::new("grpc");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    tokio::spawn(serve_grpc(svc(), addr, Some("tok".into())));
    let endpoint = format!("http://{addr}");
    let channel = loop {
        match tonic::transport::Channel::from_shared(endpoint.clone()).unwrap().connect().await {
            Ok(ch) => break ch,
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    };

    let mut anon = AtomicServiceClient::new(channel.clone());
    let e = anon.list(pb::ListRequest { arena: t.0.clone() }).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::Unauthenticated);

    let mut cl = AtomicServiceClient::with_interceptor(channel, |mut r: tonic::Request<()>| {
        r.metadata_mut().insert("authorization", "Bearer tok".parse().unwrap());
        Ok(r)
    });
    let var = || Some(pb::VarRef { arena: t.0.clone(), name: "n".into() });
    let v = |x: i64| Some(pb::Value { kind: Some(pb::value::Kind::I64(x)) });
    cl.create(pb::CreateRequest { var: var(), init: v(40), capacity: 16 }).await.unwrap();
    let r = cl
        .fetch(pb::FetchRequest { var: var(), op: pb::FetchOp::Add as i32, operand: v(2), ordering: 0, request_id: "g1".into() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!((r.previous, r.current), (v(40), v(42)));
    let r = cl
        .compare_exchange(pb::CompareExchangeRequest { var: var(), expected: v(42), desired: v(7), success: 0, failure: 0, request_id: String::new() })
        .await
        .unwrap()
        .into_inner();
    assert!(r.exchanged);
    let got = cl.get(pb::GetRequest { var: var(), ordering: 0 }).await.unwrap().into_inner();
    assert_eq!(got.value, v(7));
    assert_eq!(Arena::open(&t.0, None).unwrap().open_i64("n").unwrap().load(Ordering::SeqCst).unwrap(), 7);
    let e = cl
        .set(pb::SetRequest { var: var(), value: Some(pb::Value { kind: Some(pb::value::Kind::U64(1)) }), ordering: 0, request_id: String::new() })
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::FailedPrecondition);
    assert!(e.message().starts_with("TYPE_MISMATCH"));
    let list = cl.list(pb::ListRequest { arena: t.0.clone() }).await.unwrap().into_inner();
    assert_eq!(list.variables.len(), 1);
}

struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_atomvard"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Mcp { child, out, next_id: 1 }
    }

    fn send(&mut self, msg: J) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: J) -> J {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: J = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn call(&mut self, tool: &str, args: J) -> (bool, J) {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        let res = &r["result"];
        let text = res["content"][0]["text"].as_str().unwrap_or_default().to_string();
        let is_error = res["isError"].as_bool().unwrap_or(false);
        (is_error, serde_json::from_str(&text).unwrap_or(J::String(text)))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_stdio_session_drives_the_same_variables() {
    let t = TempArena::new("mcp");
    let mut m = Mcp::start();
    let init = m.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert!(init["result"]["instructions"].as_str().unwrap().contains("OUTCOME") || init["result"]["instructions"].as_str().unwrap().contains("UNKNOWN"));
    m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = m.request("tools/list", json!({}));
    let mut names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["atomic_add", "atomic_compare_and_set", "atomic_create", "atomic_get", "atomic_list", "atomic_set", "atomic_swap"]
    );

    let (err, r) = m.call("atomic_create", json!({"arena": t.0, "name": "jobs", "type": "i64", "init": 10}));
    assert!(!err, "{r}");
    assert_eq!(r["value"], json!("10"));
    let (_, r) = m.call("atomic_add", json!({"arena": t.0, "name": "jobs", "delta": 5, "request_id": "a1"}));
    assert_eq!(r, json!({"previous": "10", "current": "15"}));
    let (_, r) = m.call("atomic_add", json!({"arena": t.0, "name": "jobs", "delta": 5, "request_id": "a1"}));
    assert_eq!(r, json!({"previous": "10", "current": "15"}), "retry with same request_id is deduplicated");

    // a native process sees the agent's writes, and vice versa
    let native = Arena::open(&t.0, None).unwrap().open_i64("jobs").unwrap();
    assert_eq!(native.load(Ordering::SeqCst).unwrap(), 15);
    native.fetch_add(100, Ordering::SeqCst);
    let (_, r) = m.call("atomic_get", json!({"arena": t.0, "name": "jobs"}));
    assert_eq!(r["value"], json!("115"));

    let (_, r) = m.call("atomic_compare_and_set", json!({"arena": t.0, "name": "jobs", "expected": "115", "desired": "0"}));
    assert_eq!(r, json!({"exchanged": true, "previous": "115"}));
    let (err, r) = m.call("atomic_add", json!({"arena": t.0, "name": "nope", "delta": 1}));
    assert!(err, "{r}");
    assert!(r.to_string().contains("NOT_FOUND"));
    let (_, r) = m.call("atomic_list", json!({"arena": t.0}));
    assert_eq!(r["variables"][0]["name"], json!("jobs"));
}

#[test]
fn refuses_non_loopback_bind_without_token() {
    let o = Command::new(env!("CARGO_BIN_EXE_atomvard"))
        .args(["serve", "--http", "0.0.0.0:0", "--no-grpc"])
        .env_remove("ATOMVAR_TOKEN")
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("refusing to bind"));
}

/// Creates the segment file for `name` and holds an exclusive flock on it, as a
/// stuck or forked process would.
fn hold_arena_lock(name: &str) -> std::fs::File {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(format!("/dev/shm/atomvar-{name}"))
        .unwrap();
    assert_eq!(unsafe { libc_flock(f.as_raw_fd(), 2) }, 0); // LOCK_EX
    f
}

fn release_arena_lock(f: &std::fs::File) {
    use std::os::fd::AsRawFd;
    assert_eq!(unsafe { libc_flock(f.as_raw_fd(), 8) }, 0); // LOCK_UN
}

extern "C" {
    #[link_name = "flock"]
    fn libc_flock(fd: i32, op: i32) -> i32;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stuck_arena_lock_does_not_starve_the_daemon() {
    let stuck = TempArena::new("stuck");
    let other = TempArena::new("other");
    let cached = TempArena::new("cachedlocked");
    // The server gets its OWN runtime with just 2 async workers, so a regression
    // (blocking on a worker) starves the server but not this test's client,
    // which then fails cleanly on the timeouts below instead of hanging.
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    std_listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", std_listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async move {
            let svc = Service::with_limits(1000, Duration::from_secs(600), 64, 4);
            let l = tokio::net::TcpListener::from_std(std_listener).unwrap();
            axum::serve(l, http_app(svc, None, false)).await.unwrap();
        });
    });
    let c = reqwest::Client::new();
    let quick = Duration::from_secs(2);

    // an arena the daemon already has open, with one variable
    let r = c.post(format!("{base}/v1/arenas/{}/vars/x/create", cached.0)).json(&json!({"type": "i64", "init": 5})).send().await.unwrap();
    assert_eq!(r.status(), 200);

    // 1) a never-opened arena whose lock is held: flood it (2 async workers only)
    let stuck_lock = hold_arena_lock(&stuck.0);
    let flood: Vec<_> = (0..8)
        .map(|_| {
            let (c, u) = (c.clone(), format!("{base}/v1/arenas/{}/vars", stuck.0));
            tokio::spawn(async move { c.get(u).timeout(Duration::from_secs(30)).send().await.unwrap().status().as_u16() })
        })
        .collect();
    // 2) a cached arena whose lock is held: creating a NEW name blocks
    let cached_lock = hold_arena_lock(&cached.0);
    let blocked_create = {
        let (c, u) = (c.clone(), format!("{base}/v1/arenas/{}/vars/y/create", cached.0));
        tokio::spawn(async move { c.post(u).json(&json!({"type": "i64"})).timeout(Duration::from_secs(30)).send().await.unwrap().status().as_u16() })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;

    // the daemon stays responsive
    let r = c.get(format!("{base}/v1/health")).timeout(quick).send().await.expect("health must not starve");
    assert_eq!(r.status(), 200);
    let r = c.post(format!("{base}/v1/arenas/{}/vars/z/create", other.0)).json(&json!({"type": "u64"})).timeout(quick).send().await.expect("other arenas must not starve");
    assert_eq!(r.status(), 200);
    // reads of existing variables in the locked arena are lock-free
    let r = c.post(format!("{base}/v1/arenas/{}/vars/x/fetch_add", cached.0)).json(&json!({"value": 1})).timeout(quick).send().await.unwrap();
    assert_eq!(r.json::<J>().await.unwrap()["current"], json!("6"));

    release_arena_lock(&stuck_lock);
    release_arena_lock(&cached_lock);
    let mut statuses: Vec<u16> = Vec::new();
    for f in flood {
        statuses.push(f.await.unwrap());
    }
    statuses.sort();
    // 1 being served + 4 queued (per-arena cap) waited, then succeeded; 3 refused fast
    assert_eq!(statuses, vec![200, 200, 200, 200, 200, 429, 429, 429], "{statuses:?}");
    assert_eq!(blocked_create.await.unwrap(), 200);
}

async fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// Clients that disconnect while their arena's lock is held must not free the
/// gate while the blocking work continues (which would let each retry start
/// another blocking worker and exhaust the global bound for OTHER arenas).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnecting_clients_do_not_multiply_blocked_workers() {
    use atomvard::service::{OpKind, OpRequest, Operand};
    let stuck = TempArena::new("dc-stuck");
    let other = TempArena::new("dc-other");
    let svc = Service::with_limits(1000, Duration::from_secs(600), 2, 64);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = http_app(svc.clone(), None, false);
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let c = reqwest::Client::new();

    let lock = hold_arena_lock(&stuck.0);
    // 1) real HTTP clients that give up (disconnect) after 100 ms, repeatedly
    for _ in 0..8 {
        let r = c
            .get(format!("{base}/v1/arenas/{}/vars", stuck.0))
            .timeout(Duration::from_millis(100))
            .send()
            .await;
        assert!(r.is_err(), "request should time out while the lock is held");
    }
    // 2) request futures dropped directly (same thing without HTTP timing)
    for _ in 0..8 {
        let (s, a) = (svc.clone(), stuck.0.clone());
        let t = tokio::spawn(async move { s.list(&a).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        t.abort();
        let _ = t.await;
    }
    assert_eq!(svc.blocking_in_use(), 1, "exactly one worker may be parked on the stuck arena");

    // another arena is unaffected (the bound is 2 arenas)
    let create = |arena: &str, name: &str| OpRequest {
        arena: arena.into(),
        name: name.into(),
        value_type: Some(atomvar_core::ValueType::I64),
        kind: OpKind::Create { init: Some(Operand::Json(json!(1))), capacity: None },
        request_id: None,
    };
    svc.execute(create(&other.0, "v")).await.expect("other arena must not get RESOURCE_EXHAUSTED");

    release_arena_lock(&lock);
    wait_until(|| svc.blocking_in_use() == 0, "stuck worker to finish").await;
    wait_until(|| svc.gate_entries() == 0, "gate entries to be reclaimed").await;
    svc.execute(create(&stuck.0, "v")).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_table_does_not_grow_with_rejected_or_missing_names() {
    use atomvard::service::{OpKind, OpRequest, Operand};
    let svc = svc();
    for i in 0..2000 {
        let bad = format!("bad/{i}/{}", "x".repeat(2000));
        let e = svc.list(&bad).await.unwrap_err();
        assert_eq!(e.code, atomvard::error::ErrorCode::InvalidName);
        let e = svc
            .execute(OpRequest {
                arena: bad,
                name: "v".into(),
                value_type: Some(atomvar_core::ValueType::I64),
                kind: OpKind::Create { init: Some(Operand::Json(json!(0))), capacity: None },
                request_id: None,
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, atomvard::error::ErrorCode::InvalidName);
    }
    for i in 0..500 {
        let e = svc.list(&format!("missing-{}-{i}", std::process::id())).await.unwrap_err();
        assert_eq!(e.code, atomvard::error::ErrorCode::NotFound);
    }
    assert_eq!(svc.gate_entries(), 0);
}
