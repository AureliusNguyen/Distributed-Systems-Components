use crate::error::{Error, Result};
use std::sync::atomic::Ordering as StdOrdering;

/// Memory ordering, mirroring C++11 / Rust. `SeqCst` is the default in every
/// binding and matches Java `AtomicLong` semantics.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Ordering {
    Relaxed = 0,
    Acquire = 1,
    Release = 2,
    AcqRel = 3,
    #[default]
    SeqCst = 4,
}

impl Ordering {
    pub fn from_u8(v: u8) -> Result<Self> {
        Ok(match v {
            0 => Self::Relaxed,
            1 => Self::Acquire,
            2 => Self::Release,
            3 => Self::AcqRel,
            4 => Self::SeqCst,
            _ => return Err(Error::InvalidOrdering("unknown ordering value")),
        })
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "relaxed" => Self::Relaxed,
            "acquire" => Self::Acquire,
            "release" => Self::Release,
            "acq_rel" | "acqrel" => Self::AcqRel,
            "seq_cst" | "seqcst" => Self::SeqCst,
            _ => return Err(Error::InvalidOrdering("unknown ordering name")),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relaxed => "relaxed",
            Self::Acquire => "acquire",
            Self::Release => "release",
            Self::AcqRel => "acq_rel",
            Self::SeqCst => "seq_cst",
        }
    }

    pub(crate) fn std(self) -> StdOrdering {
        match self {
            Self::Relaxed => StdOrdering::Relaxed,
            Self::Acquire => StdOrdering::Acquire,
            Self::Release => StdOrdering::Release,
            Self::AcqRel => StdOrdering::AcqRel,
            Self::SeqCst => StdOrdering::SeqCst,
        }
    }

    /// Orderings valid for a plain load.
    pub(crate) fn for_load(self) -> Result<StdOrdering> {
        match self {
            Self::Release => Err(Error::InvalidOrdering("a load cannot use release")),
            Self::AcqRel => Err(Error::InvalidOrdering("a load cannot use acq_rel")),
            o => Ok(o.std()),
        }
    }

    /// Orderings valid for a plain store.
    pub(crate) fn for_store(self) -> Result<StdOrdering> {
        match self {
            Self::Acquire => Err(Error::InvalidOrdering("a store cannot use acquire")),
            Self::AcqRel => Err(Error::InvalidOrdering("a store cannot use acq_rel")),
            o => Ok(o.std()),
        }
    }

    /// Strength of the load half: relaxed=0, acquire=1, seq_cst=2.
    fn load_rank(self) -> u8 {
        match self {
            Self::Relaxed | Self::Release => 0,
            Self::Acquire | Self::AcqRel => 1,
            Self::SeqCst => 2,
        }
    }

    /// Validates a compare-exchange ordering pair. The failure ordering must be a
    /// load ordering and may not be stronger than the success ordering's load half
    /// (the C++11 rule; stricter than current Rust, kept for cross-language parity).
    pub(crate) fn for_cas(success: Self, failure: Self) -> Result<(StdOrdering, StdOrdering)> {
        match failure {
            Self::Release => {
                return Err(Error::InvalidOrdering(
                    "compare_exchange failure ordering cannot be release",
                ))
            }
            Self::AcqRel => {
                return Err(Error::InvalidOrdering(
                    "compare_exchange failure ordering cannot be acq_rel",
                ))
            }
            _ => {}
        }
        if failure.load_rank() > success.load_rank() {
            return Err(Error::InvalidOrdering(
                "compare_exchange failure ordering is stronger than success ordering",
            ));
        }
        Ok((success.std(), failure.std()))
    }

    /// The strongest failure ordering allowed for this success ordering.
    pub fn default_failure(self) -> Self {
        match self {
            Self::Relaxed | Self::Release => Self::Relaxed,
            Self::Acquire | Self::AcqRel => Self::Acquire,
            Self::SeqCst => Self::SeqCst,
        }
    }
}
