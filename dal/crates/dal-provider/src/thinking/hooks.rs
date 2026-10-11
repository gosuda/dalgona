//! The `before_request` hook seam: session level plus hook patches.
//!
//! Hooks run in registration order; a later hook's `Some` field replaces an
//! earlier one. The agent loop and host part passes hooks, notices, and the
//! turn token; this module only folds their patches.

use std::sync::Arc;

use dal_core::ThinkingLevel;
use futures::future::BoxFuture;

use super::level::Effort;
use crate::error::ProviderError;

/// What a `before_request` hook sees.
#[derive(Clone, Copy, Debug)]
pub struct BeforeRequestInput<'a> {
    /// The model id of the request.
    pub model: &'a str,
    /// The session level, before clamping.
    pub level: ThinkingLevel,
}

/// The typed parameters a `before_request` hook may set; `None` keeps the
/// value composed so far.
#[derive(Clone, Debug, Default)]
pub struct BeforeRequestPatch {
    /// Replaces the level-derived effort on families with an effort field.
    pub effort: Option<Effort>,
    /// A sampling temperature, sent only when [`super::wire::plan`] allows it.
    pub temperature: Option<f32>,
}

/// A Rust or Starlark hook run before each provider request.
pub trait BeforeRequest: Send + Sync {
    /// Returns the parameters this hook sets for one request.
    fn before_request<'a>(
        &'a self,
        input: BeforeRequestInput<'a>,
    ) -> BoxFuture<'a, Result<BeforeRequestPatch, ProviderError>>;
}

/// Runs `hooks` in registration order; a later hook's `Some` field replaces
/// an earlier one.
///
/// # Errors
///
/// The first hook error, unchanged; later hooks do not run.
pub async fn compose_hooks(
    hooks: &[Arc<dyn BeforeRequest>],
    model: &str,
    level: ThinkingLevel,
) -> Result<BeforeRequestPatch, ProviderError> {
    let mut merged = BeforeRequestPatch::default();
    for hook in hooks {
        let patch = hook
            .before_request(BeforeRequestInput { model, level })
            .await?;
        merged.effort = patch.effort.or(merged.effort);
        merged.temperature = patch.temperature.or(merged.temperature);
    }
    Ok(merged)
}
