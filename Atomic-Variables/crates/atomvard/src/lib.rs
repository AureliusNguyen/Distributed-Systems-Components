//! atomvard: serves atomvar's named atomic variables over gRPC, HTTP/JSON and
//! MCP. All three surfaces share one handler layer (`service`).

pub mod error;
pub mod grpc;
pub mod http;
pub mod idem;
pub mod mcp;
pub mod service;
pub mod wire;

use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use std::net::SocketAddr;
use std::sync::Arc;

pub use service::Service;

/// Builds the HTTP router: JSON API under /v1 and MCP (streamable HTTP) at
/// /mcp, both behind the optional bearer token.
pub fn http_app(svc: Arc<Service>, token: Option<Arc<str>>, mcp: bool) -> axum::Router {
    let mut app = http::router(svc.clone());
    if mcp {
        let mcp_svc: StreamableHttpService<mcp::McpServer, LocalSessionManager> = StreamableHttpService::new(
            move || Ok(mcp::McpServer::new(svc.clone())),
            Default::default(),
            StreamableHttpServerConfig::default(),
        );
        app = app.nest_service("/mcp", mcp_svc);
    }
    app.layer(axum::middleware::from_fn_with_state(token, http::auth))
}

/// gRPC server with the optional bearer-token check.
pub async fn serve_grpc(svc: Arc<Service>, addr: SocketAddr, token: Option<Arc<str>>) -> Result<(), tonic::transport::Error> {
    let check = move |req: tonic::Request<()>| -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(t) = &token {
            let want = format!("Bearer {t}");
            let got = req
                .metadata()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !http::constant_time_eq(got.as_bytes(), want.as_bytes()) {
                return Err(tonic::Status::unauthenticated("missing or invalid bearer token"));
            }
        }
        Ok(req)
    };
    let server = grpc::pb::atomic_service_server::AtomicServiceServer::with_interceptor(grpc::Grpc { svc }, check);
    tonic::transport::Server::builder().add_service(server).serve(addr).await
}
