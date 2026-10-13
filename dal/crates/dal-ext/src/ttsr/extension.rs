//! The thin `ttsr` extension record for product assembly.
//!
//! [`extension`] name-registers the extension in the fixed 12-extension
//! order. Rule loading, reports, and stream watching attach separately:
//! the product feeds records and files through [`super::build::RuleBuildInput`]
//! and registers [`super::watch::factory::TtsrWatchFactory`] through
//! `ExtensionBuilder::output_stream`, which owns the runtime watcher list.

use dal_agent::ext::{Extension, ExtensionBuilder};
use dal_core::{RegistrationError, ServiceSet};

/// Builds the thin `ttsr` extension record: name registration only.
///
/// # Errors
///
/// Fails with [`RegistrationError`] when the builder rejects the extension
/// name.
pub fn extension() -> Result<Extension, RegistrationError> {
    ExtensionBuilder::new("ttsr", "0.1.0", ServiceSet::EMPTY)?.build()
}
