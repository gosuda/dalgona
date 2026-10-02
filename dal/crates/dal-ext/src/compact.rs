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
/// # Errors
///
/// Returns the builder's registration error for an invalid identity.
pub fn extension() -> Result<Extension, RegistrationError> {
    ExtensionBuilder::new("compact", env!("CARGO_PKG_VERSION"), ServiceSet::default())?
        .compactor("remote", Arc::new(remote::Remote))
        .compactor("summary", Arc::new(summary::Summary))
        .build()
}

#[cfg(test)]
mod tests {
    use super::extension;

    #[test]
    fn registers_remote_before_summary() {
        let extension = extension().expect("the built-in compact extension is valid");
        let compactors = extension.compactors();
        assert_eq!(compactors.len(), 2);
        assert_eq!(compactors[0].0.as_ref(), "remote");
        assert_eq!(compactors[1].0.as_ref(), "summary");
    }
}
