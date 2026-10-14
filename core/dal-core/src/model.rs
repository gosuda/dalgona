//! Provider-neutral model values, raw-bearing stream parts, and token pricing.
//!
//! Routes describe a destination; provider lookup and alias resolution belong
//! to the provider layer. Prices are USD per million tokens. A reported cost
//! is authoritative, while absent or unusable prices remain unknown.

use std::sync::Arc;
use std::time::Duration;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::content::Part;
use crate::id::CallId;
use crate::raw::{RawJson, Tagged};

mod core;
mod params;
mod parts;
mod pricing;

pub use core::{
    Family, MAX_SYNTHETIC_DEPTH, ModelRoute, ReplaySource, RouteError, ThinkingLevel,
    check_synthetic_chain,
};
pub use params::{Caps, ContextItem, ModelInfo, ModelToolSpec, Purpose, RequestParams};
pub use parts::{AssistantPart, Stop, StreamEvent};
pub use pricing::{
    CompactOutcome, CompactedHistory, InferFailure, Inference, ModelPrice, ModelRequest, PriceTier,
    StreamChannel, Usage,
};

#[cfg(test)]
mod tests;
