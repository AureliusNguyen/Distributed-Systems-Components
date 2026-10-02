//! HTTP/JSON surface (see openapi.yaml). Mirrors proto/atomvar.proto.

use crate::error::{ApiError, ErrorCode};
use crate::service::{vars_to_json, FetchOp, OpKind, OpRequest, Operand, Service};
use atomvar_core::{Ordering, ValueType};
use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use std::sync::Arc;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(json!({"error": {"code": self.code.as_str(), "message": self.message}}))).into_response()
    }
}

pub fn router(svc: Arc<Service>) -> Router {
    Router::new()
        .route("/v1/health", get(|| async { Json(json!({"ok": true})) }))
        .route("/v1/arenas/{arena}/vars", get(list))
        .route("/v1/arenas/{arena}/vars/{name}", get(get_var))
        .route("/v1/arenas/{arena}/vars/{name}/{op}", post(op))
        .with_state(svc)
}

/// Bearer-token check applied to every route (HTTP API and /mcp) when a token
/// is configured.
pub async fn auth(State(token): State<Option<Arc<str>>>, req: Request, next: Next) -> Response {
    if let Some(token) = token {
        let want = format!("Bearer {token}");
        let got = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(got.as_bytes(), want.as_bytes()) {
            return ApiError::new(ErrorCode::Unauthenticated, "missing or invalid bearer token").into_response();
        }
    }
    next.run(req).await
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OpBody {
    #[serde(rename = "type")]
    value_type: Option<String>,
    value: Option<JsonValue>,
    init: Option<JsonValue>,
    expected: Option<JsonValue>,
    desired: Option<JsonValue>,
    ordering: Option<String>,
    success: Option<String>,
    failure: Option<String>,
    capacity: Option<u32>,
    request_id: Option<String>,
}

#[derive(Deserialize)]
struct GetQuery {
    ordering: Option<String>,
}

fn ordering(s: &Option<String>) -> Result<Ordering, ApiError> {
    match s {
        None => Ok(Ordering::SeqCst),
        Some(s) => Ok(Ordering::parse(s)?),
    }
}

fn required(v: Option<JsonValue>, field: &str) -> Result<Operand, ApiError> {
    v.map(Operand::Json)
        .ok_or_else(|| ApiError::invalid(format!("missing field '{field}'")))
}

async fn list(State(svc): State<Arc<Service>>, Path(arena): Path<String>) -> Result<Json<JsonValue>, ApiError> {
    let vars = svc.list(&arena).await?;
    Ok(Json(vars_to_json(&vars)))
}

async fn get_var(
    State(svc): State<Arc<Service>>,
    Path((arena, name)): Path<(String, String)>,
    Query(q): Query<GetQuery>,
) -> Result<Json<JsonValue>, ApiError> {
    let req = OpRequest {
        arena,
        name,
        value_type: None,
        kind: OpKind::Get {
            ordering: ordering(&q.ordering)?,
        },
        request_id: None,
    };
    Ok(Json(svc.execute(req).await?.to_json()))
}

async fn op(
    State(svc): State<Arc<Service>>,
    Path((arena, name, op)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<JsonValue>, ApiError> {
    let b: OpBody = if body.is_empty() {
        OpBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::invalid(format!("invalid JSON body: {e}")))?
    };
    let value_type = b.value_type.as_deref().map(ValueType::parse).transpose()?;
    let request_id = b.request_id.clone().or_else(|| {
        headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    });
    let kind = match op.as_str() {
        "create" => OpKind::Create {
            init: b.init.or(b.value).map(Operand::Json),
            capacity: b.capacity,
        },
        "get" | "load" => OpKind::Get {
            ordering: ordering(&b.ordering)?,
        },
        "set" | "store" => OpKind::Set {
            value: required(b.value, "value")?,
            ordering: ordering(&b.ordering)?,
        },
        "swap" | "get_and_set" => OpKind::Swap {
            value: required(b.value, "value")?,
            ordering: ordering(&b.ordering)?,
        },
        "compare_exchange" | "compare_and_set" => OpKind::CompareExchange {
            expected: required(b.expected, "expected")?,
            desired: required(b.desired, "desired")?,
            success: ordering(&b.success)?,
            failure: b.failure.as_deref().map(Ordering::parse).transpose()?,
        },
        other => match FetchOp::parse(other) {
            Some(f) if other.starts_with("fetch_") => OpKind::Fetch {
                op: f,
                operand: required(b.value, "value")?,
                ordering: ordering(&b.ordering)?,
            },
            _ => return Err(ApiError::new(ErrorCode::NotFound, format!("unknown operation '{other}'"))),
        },
    };
    let req = OpRequest {
        arena,
        name,
        value_type,
        kind,
        request_id,
    };
    Ok(Json(svc.execute(req).await?.to_json()))
}
