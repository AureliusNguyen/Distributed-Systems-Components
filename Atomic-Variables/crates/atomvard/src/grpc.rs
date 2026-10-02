//! gRPC surface generated from proto/atomvar.proto.

use crate::error::ApiError;
use crate::service::{FetchOp, OpKind, OpRequest, OpResult, Operand, Service};
use atomvar_core::{Ordering, Value, ValueType};
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub mod pb {
    tonic::include_proto!("atomvar.v1");
}

use pb::atomic_service_server::AtomicService;

pub fn value_to_pb(v: &Value) -> pb::Value {
    use pb::value::Kind;
    let kind = match *v {
        Value::I64(x) => Kind::I64(x),
        Value::U64(x) => Kind::U64(x),
        Value::Bool(x) => Kind::Bool(x),
        Value::F64(x) => Kind::F64(pb::F64 {
            value: x,
            bits: Some(x.to_bits()),
        }),
        Value::U128(x) => Kind::U128(pb::U128 {
            lo: x as u64,
            hi: (x >> 64) as u64,
        }),
    };
    pb::Value { kind: Some(kind) }
}

pub fn value_from_pb(v: Option<pb::Value>, field: &str) -> Result<Value, ApiError> {
    use pb::value::Kind;
    match v.and_then(|v| v.kind) {
        Some(Kind::I64(x)) => Ok(Value::I64(x)),
        Some(Kind::U64(x)) => Ok(Value::U64(x)),
        Some(Kind::Bool(x)) => Ok(Value::Bool(x)),
        Some(Kind::F64(f)) => Ok(Value::F64(match f.bits {
            Some(bits) => f64::from_bits(bits),
            None => f.value,
        })),
        Some(Kind::U128(u)) => Ok(Value::U128((u.hi as u128) << 64 | u.lo as u128)),
        None => Err(ApiError::invalid(format!("missing field '{field}'"))),
    }
}

pub fn type_to_pb(t: ValueType) -> pb::ValueType {
    match t {
        ValueType::I64 => pb::ValueType::I64,
        ValueType::U64 => pb::ValueType::U64,
        ValueType::Bool => pb::ValueType::Bool,
        ValueType::F64 => pb::ValueType::F64,
        ValueType::U128 => pb::ValueType::U128,
    }
}

/// UNSPECIFIED -> None (caller decides the default).
fn ordering_from_pb(v: i32) -> Result<Option<Ordering>, ApiError> {
    Ok(match pb::Ordering::try_from(v).map_err(|_| ApiError::invalid("unknown ordering"))? {
        pb::Ordering::Unspecified => None,
        pb::Ordering::Relaxed => Some(Ordering::Relaxed),
        pb::Ordering::Acquire => Some(Ordering::Acquire),
        pb::Ordering::Release => Some(Ordering::Release),
        pb::Ordering::AcqRel => Some(Ordering::AcqRel),
        pb::Ordering::SeqCst => Some(Ordering::SeqCst),
    })
}

fn ord(v: i32) -> Result<Ordering, ApiError> {
    Ok(ordering_from_pb(v)?.unwrap_or(Ordering::SeqCst))
}

fn var(v: Option<pb::VarRef>) -> Result<(String, String), ApiError> {
    let v = v.ok_or_else(|| ApiError::invalid("missing field 'var'"))?;
    Ok((v.arena, v.name))
}

fn rid(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

pub struct Grpc {
    pub svc: Arc<Service>,
}

impl Grpc {
    async fn run(&self, arena: String, name: String, kind: OpKind, request_id: Option<String>) -> Result<OpResult, Status> {
        let req = OpRequest {
            arena,
            name,
            value_type: None,
            kind,
            request_id,
        };
        Ok(self.svc.execute(req).await?)
    }
}

fn unexpected() -> Status {
    Status::internal("unexpected result shape")
}

#[tonic::async_trait]
impl AtomicService for Grpc {
    async fn create(&self, r: Request<pb::CreateRequest>) -> Result<Response<pb::ValueResponse>, Status> {
        let r = r.into_inner();
        let (arena, name) = var(r.var)?;
        let init = value_from_pb(r.init, "init")?;
        let kind = OpKind::Create {
            init: Some(Operand::Typed(init)),
            capacity: (r.capacity != 0).then_some(r.capacity),
        };
        match self.run(arena, name, kind, None).await? {
            OpResult::Value { value } => Ok(Response::new(pb::ValueResponse {
                r#type: type_to_pb(value.value_type()) as i32,
                value: Some(value_to_pb(&value)),
            })),
            _ => Err(unexpected()),
        }
    }

    async fn get(&self, r: Request<pb::GetRequest>) -> Result<Response<pb::ValueResponse>, Status> {
        let r = r.into_inner();
        let (arena, name) = var(r.var)?;
        let kind = OpKind::Get { ordering: ord(r.ordering)? };
        match self.run(arena, name, kind, None).await? {
            OpResult::Value { value } => Ok(Response::new(pb::ValueResponse {
                r#type: type_to_pb(value.value_type()) as i32,
                value: Some(value_to_pb(&value)),
            })),
            _ => Err(unexpected()),
        }
    }

    async fn set(&self, r: Request<pb::SetRequest>) -> Result<Response<pb::SetResponse>, Status> {
        let r = r.into_inner();
        let (arena, name) = var(r.var)?;
        let kind = OpKind::Set {
            value: Operand::Typed(value_from_pb(r.value, "value")?),
            ordering: ord(r.ordering)?,
        };
        match self.run(arena, name, kind, rid(r.request_id)).await? {
            OpResult::Stored => Ok(Response::new(pb::SetResponse {})),
            _ => Err(unexpected()),
        }
    }

    async fn swap(&self, r: Request<pb::SwapRequest>) -> Result<Response<pb::SwapResponse>, Status> {
        let r = r.into_inner();
        let (arena, name) = var(r.var)?;
        let kind = OpKind::Swap {
            value: Operand::Typed(value_from_pb(r.value, "value")?),
            ordering: ord(r.ordering)?,
        };
        match self.run(arena, name, kind, rid(r.request_id)).await? {
            OpResult::Swapped { previous } => Ok(Response::new(pb::SwapResponse {
                previous: Some(value_to_pb(&previous)),
            })),
            _ => Err(unexpected()),
        }
    }

    async fn compare_exchange(
        &self,
        r: Request<pb::CompareExchangeRequest>,
    ) -> Result<Response<pb::CompareExchangeResponse>, Status> {
        let r = r.into_inner();
        let (arena, name) = var(r.var)?;
        let kind = OpKind::CompareExchange {
            expected: Operand::Typed(value_from_pb(r.expected, "expected")?),
            desired: Operand::Typed(value_from_pb(r.desired, "desired")?),
            success: ord(r.success)?,
            failure: ordering_from_pb(r.failure)?,
        };
        match self.run(arena, name, kind, rid(r.request_id)).await? {
            OpResult::Cas { exchanged, previous } => Ok(Response::new(pb::CompareExchangeResponse {
                exchanged,
                previous: Some(value_to_pb(&previous)),
            })),
            _ => Err(unexpected()),
        }
    }

    async fn fetch(&self, r: Request<pb::FetchRequest>) -> Result<Response<pb::FetchResponse>, Status> {
        let r = r.into_inner();
        let (arena, name) = var(r.var)?;
        let op = match pb::FetchOp::try_from(r.op).map_err(|_| ApiError::invalid("unknown fetch op"))? {
            pb::FetchOp::Unspecified => return Err(ApiError::invalid("fetch op is required").into()),
            pb::FetchOp::Add => FetchOp::Add,
            pb::FetchOp::Sub => FetchOp::Sub,
            pb::FetchOp::And => FetchOp::And,
            pb::FetchOp::Or => FetchOp::Or,
            pb::FetchOp::Xor => FetchOp::Xor,
            pb::FetchOp::Max => FetchOp::Max,
            pb::FetchOp::Min => FetchOp::Min,
            pb::FetchOp::Nand => FetchOp::Nand,
        };
        let kind = OpKind::Fetch {
            op,
            operand: Operand::Typed(value_from_pb(r.operand, "operand")?),
            ordering: ord(r.ordering)?,
        };
        match self.run(arena, name, kind, rid(r.request_id)).await? {
            OpResult::Fetched { previous, current } => Ok(Response::new(pb::FetchResponse {
                previous: Some(value_to_pb(&previous)),
                current: Some(value_to_pb(&current)),
            })),
            _ => Err(unexpected()),
        }
    }

    async fn list(&self, r: Request<pb::ListRequest>) -> Result<Response<pb::ListResponse>, Status> {
        let vars = self.svc.list(&r.into_inner().arena).await?;
        Ok(Response::new(pb::ListResponse {
            variables: vars
                .into_iter()
                .map(|v| pb::VarInfo {
                    name: v.name,
                    r#type: type_to_pb(v.value_type) as i32,
                    slot: v.slot,
                })
                .collect(),
        }))
    }
}
