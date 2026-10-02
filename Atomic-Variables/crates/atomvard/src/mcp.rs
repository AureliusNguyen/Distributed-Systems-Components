//! MCP surface: the same named atomic variables as tools for AI agents.
//! All operations use SEQ_CST ordering.

use crate::service::{vars_to_json, FetchOp, OpKind, OpRequest, Operand, Service};
use atomvar_core::{Ordering, ValueType};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::sync::Arc;

const INSTRUCTIONS: &str = "\
atomvar: named lock-free atomic variables in shared memory on this host. The same \
variable is visible to native programs (Python, C, Rust) that open the same arena and \
name, so these tools interoperate with them. Every operation is one atomic operation \
(SEQ_CST).

Rules for safe use:
1. A timeout or lost response means the outcome is UNKNOWN. Never blindly retry a \
mutation. Pass a unique request_id: a retry with the same request_id returns the \
original result instead of applying the change twice, but only on this same daemon \
and only while the entry is retained (default 10 minutes).
2. compare_and_set is not automatically retry-safe: if the value went A -> B -> A, \
a retried CAS(A, X) succeeds again (ABA). Retry-safe patterns: (a) irreversible claims \
(0 -> your unique token, never reset); (b) CAS on values that never repeat, such as a \
version counter you increment on every write.
3. CAS gives exactly one winner for a claim within this store. It is NOT leader \
election with failover: there are no leases or fencing.
4. Integers are returned as decimal strings (exact for 64/128-bit). You may pass \
numbers or strings. Floats come back as {value, bits}; pass {\"bits\": \"0x...\"} for \
exact bit patterns.";

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CreateArgs {
    /// Arena name ([A-Za-z0-9._-]); created with the default capacity if missing.
    pub arena: String,
    /// Variable name (1-64 bytes).
    pub name: String,
    /// One of: i64, u64, bool, f64, u128.
    #[serde(rename = "type")]
    pub value_type: String,
    /// Initial value, used only if this call creates the variable. Defaults to zero/false.
    pub init: Option<JsonValue>,
    /// Arena capacity (number of variables), only when creating a new arena.
    pub capacity: Option<u32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct VarArgs {
    pub arena: String,
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetArgs {
    pub arena: String,
    pub name: String,
    /// New value (number, decimal string, bool, or {"bits": "0x..."} for f64).
    pub value: JsonValue,
    /// Unique id for this logical write; reuse it only when retrying the same write.
    pub request_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    pub arena: String,
    pub name: String,
    /// Amount to add (negative to subtract for i64/f64). Integers wrap on overflow.
    pub delta: JsonValue,
    /// Unique id for this logical increment; reuse it only when retrying the same increment.
    pub request_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CasArgs {
    pub arena: String,
    pub name: String,
    /// Value the variable must currently hold (f64 compares exact bits).
    pub expected: JsonValue,
    /// Value to store if it does.
    pub desired: JsonValue,
    /// Unique id for this logical attempt; reuse it only when retrying the same attempt.
    pub request_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    pub arena: String,
}

#[derive(Clone)]
pub struct McpServer {
    svc: Arc<Service>,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    pub fn new(svc: Arc<Service>) -> Self {
        McpServer {
            svc,
            tool_router: Self::tool_router(),
        }
    }

    async fn run(&self, arena: String, name: String, kind: OpKind, request_id: Option<String>) -> Result<String, String> {
        let req = OpRequest {
            arena,
            name,
            value_type: None,
            kind,
            request_id,
        };
        self.svc
            .execute(req)
            .await
            .map(|r| r.to_json().to_string())
            .map_err(|e| e.to_string())
    }
}

#[tool_router]
impl McpServer {
    #[tool(description = "Open or create a named atomic variable (types: i64, u64, bool, f64, u128). \
Idempotent: if it already exists with the same type, returns its current value and ignores init.")]
    async fn atomic_create(&self, Parameters(a): Parameters<CreateArgs>) -> Result<String, String> {
        let ty = ValueType::parse(&a.value_type).map_err(|e| e.to_string())?;
        let req = OpRequest {
            arena: a.arena,
            name: a.name,
            value_type: Some(ty),
            kind: OpKind::Create {
                init: a.init.map(Operand::Json),
                capacity: a.capacity,
            },
            request_id: None,
        };
        self.svc
            .execute(req)
            .await
            .map(|r| r.to_json().to_string())
            .map_err(|e| e.to_string())
    }

    #[tool(description = "Read a variable's current value (atomic SEQ_CST load).")]
    async fn atomic_get(&self, Parameters(a): Parameters<VarArgs>) -> Result<String, String> {
        self.run(a.arena, a.name, OpKind::Get { ordering: Ordering::SeqCst }, None).await
    }

    #[tool(description = "Atomically overwrite a variable's value. Pass a unique request_id so a retry \
after a lost response is not applied twice.")]
    async fn atomic_set(&self, Parameters(a): Parameters<SetArgs>) -> Result<String, String> {
        let kind = OpKind::Set {
            value: Operand::Json(a.value),
            ordering: Ordering::SeqCst,
        };
        self.run(a.arena, a.name, kind, a.request_id).await
    }

    #[tool(description = "Atomically add delta to an i64, u64 or f64 variable (fetch_add). Returns \
{previous, current}. Integers wrap on overflow. Pass a unique request_id; NEVER retry an add without \
the same request_id, or it may be applied twice.")]
    async fn atomic_add(&self, Parameters(a): Parameters<AddArgs>) -> Result<String, String> {
        let kind = OpKind::Fetch {
            op: FetchOp::Add,
            operand: Operand::Json(a.delta),
            ordering: Ordering::SeqCst,
        };
        self.run(a.arena, a.name, kind, a.request_id).await
    }

    #[tool(description = "Compare-and-set: store desired only if the variable currently equals expected. \
Returns {exchanged, previous}. Exactly one concurrent caller can win a claim (e.g. 0 -> your unique \
token, never reset). Not retry-safe if the value can return to expected (ABA); not leader election \
with failover (no leases).")]
    async fn atomic_compare_and_set(&self, Parameters(a): Parameters<CasArgs>) -> Result<String, String> {
        let kind = OpKind::CompareExchange {
            expected: Operand::Json(a.expected),
            desired: Operand::Json(a.desired),
            success: Ordering::SeqCst,
            failure: None,
        };
        self.run(a.arena, a.name, kind, a.request_id).await
    }

    #[tool(description = "Atomically replace a variable's value and return the previous one. Pass a \
unique request_id so a retry is not applied twice.")]
    async fn atomic_swap(&self, Parameters(a): Parameters<SetArgs>) -> Result<String, String> {
        let kind = OpKind::Swap {
            value: Operand::Json(a.value),
            ordering: Ordering::SeqCst,
        };
        self.run(a.arena, a.name, kind, a.request_id).await
    }

    #[tool(description = "List the variables (name, type) in an arena.")]
    async fn atomic_list(&self, Parameters(a): Parameters<ListArgs>) -> Result<String, String> {
        self.svc
            .list(&a.arena)
            .await
            .map(|v| vars_to_json(&v).to_string())
            .map_err(|e| e.to_string())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(INSTRUCTIONS)
    }
}
