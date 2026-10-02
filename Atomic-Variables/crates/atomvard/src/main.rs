use atomvard::{http_app, mcp::McpServer, serve_grpc, Service};
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "atomvard", version, about = "Named lock-free atomic variables over gRPC, HTTP/JSON and MCP")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
struct IdemOpts {
    /// Seconds a completed request_id is retained for deduplication.
    #[arg(long, default_value_t = 600)]
    idem_ttl_secs: u64,
    /// Maximum retained request_ids.
    #[arg(long, default_value_t = 100_000)]
    idem_capacity: usize,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve HTTP/JSON (+ MCP at /mcp) and gRPC.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7878")]
        http: SocketAddr,
        #[arg(long, default_value = "127.0.0.1:7879")]
        grpc: SocketAddr,
        /// Disable the HTTP listener (and /mcp).
        #[arg(long)]
        no_http: bool,
        /// Disable the gRPC listener.
        #[arg(long)]
        no_grpc: bool,
        /// Do not mount MCP (streamable HTTP) at /mcp.
        #[arg(long)]
        no_mcp: bool,
        /// Bearer token required on every request.
        #[arg(long, env = "ATOMVAR_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Allow binding a non-loopback address without a token.
        #[arg(long)]
        insecure_no_auth: bool,
        #[command(flatten)]
        idem: IdemOpts,
    },
    /// Serve MCP over stdio (for `claude mcp add atomvar -- atomvard mcp`).
    Mcp {
        #[command(flatten)]
        idem: IdemOpts,
    },
    /// Arena administration.
    Arena {
        #[command(subcommand)]
        cmd: ArenaCmd,
    },
}

#[derive(Subcommand)]
enum ArenaCmd {
    /// List the variables in an arena.
    Inspect { name: String },
    /// Delete an arena's shared-memory segment. DANGER: processes that still have
    /// it mapped keep using the orphaned copy while new openers get a fresh one.
    /// Only run when nothing is using the arena.
    Destroy {
        name: String,
        /// Confirm you understand the split-brain hazard.
        #[arg(long)]
        yes: bool,
    },
}

fn service(o: &IdemOpts) -> Arc<Service> {
    Service::new(o.idem_capacity, Duration::from_secs(o.idem_ttl_secs))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Err(e) = atomvar_core::init() {
        eprintln!("warning: {e}");
    }
    match Cli::parse().cmd {
        Cmd::Serve {
            http,
            grpc,
            no_http,
            no_grpc,
            no_mcp,
            token,
            insecure_no_auth,
            idem,
        } => {
            let exposed = (!no_http && !http.ip().is_loopback()) || (!no_grpc && !grpc.ip().is_loopback());
            if exposed && token.is_none() && !insecure_no_auth {
                return Err("refusing to bind a non-loopback address without --token (or --insecure-no-auth)".into());
            }
            let token: Option<Arc<str>> = token.map(Into::into);
            let svc = service(&idem);
            let mut tasks = tokio::task::JoinSet::new();
            if !no_http {
                let listener = tokio::net::TcpListener::bind(http).await?;
                eprintln!("atomvard: HTTP/JSON on http://{http}/v1{}", if no_mcp { "" } else { ", MCP at /mcp" });
                let app = http_app(svc.clone(), token.clone(), !no_mcp);
                tasks.spawn(async move { axum::serve(listener, app).await.map_err(|e| e.to_string()) });
            }
            if !no_grpc {
                eprintln!("atomvard: gRPC on {grpc}");
                let (svc, token) = (svc.clone(), token.clone());
                tasks.spawn(async move { serve_grpc(svc, grpc, token).await.map_err(|e| e.to_string()) });
            }
            if tasks.is_empty() {
                return Err("nothing to serve (--no-http and --no-grpc)".into());
            }
            if let Some(r) = tasks.join_next().await {
                r??;
            }
        }
        Cmd::Mcp { idem } => {
            let server = McpServer::new(service(&idem)).serve(rmcp::transport::stdio()).await?;
            server.waiting().await?;
        }
        Cmd::Arena { cmd } => match cmd {
            ArenaCmd::Inspect { name } => {
                if !atomvar_core::arena_exists(&name)? {
                    return Err(format!("arena '{name}' not found").into());
                }
                let arena = atomvar_core::Arena::open(&name, None)?;
                println!("arena {name}: capacity {}", arena.capacity());
                for v in arena.list()? {
                    let value = arena.open_var(&v.name, v.value_type)?.load(atomvar_core::Ordering::SeqCst)?;
                    println!("  {:<24} {:<5} {}", v.name, v.value_type.as_str(), atomvard::wire::encode(&value));
                }
            }
            ArenaCmd::Destroy { name, yes } => {
                if !yes {
                    return Err("pass --yes: destroying an arena that is still in use causes split brain".into());
                }
                atomvar_core::destroy_arena(&name)?;
                println!("destroyed arena {name}");
            }
        },
    }
    Ok(())
}
