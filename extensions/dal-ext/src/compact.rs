mod remote;
mod summary;

use std::sync::Arc;

use dal_agent::ext::{Extension, ExtensionBuilder};
use dal_core::{Inference, RegistrationError, ServiceSet, StreamEvent, Usage};

/// Returns the compaction request usage when the provider reported one.
///
/// A trailing usage event is normal traffic beside the terminal result; it
/// never decides the outcome, only fills the journal usage record.
pub(super) fn usage_of(inference: &Inference) -> Option<Usage> {
    inference.events.iter().find_map(|event| match event {
        StreamEvent::Usage(usage) => Some(*usage),
        _ => None,
    })
}

/// Registers dal's native-first, text-summary compaction chain.
///
/// `remote` is a primary compactor and `summary` a fallback compactor, so
/// every other extension's primary compactor, such as a bundled battery's,
/// runs between them: `remote`, then the batteries, then `summary`.
///
/// # Errors
///
/// Fails with [`RegistrationError`] when the builder rejects the extension
/// name or one of the compactors.
pub fn extension() -> Result<Extension, RegistrationError> {
    ExtensionBuilder::new("compact", env!("CARGO_PKG_VERSION"), ServiceSet::default())?
        .compactor("remote", Arc::new(remote::Remote))
        .fallback_compactor("summary", Arc::new(summary::Summary))
        .build()
}
