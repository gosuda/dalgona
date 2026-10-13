//! Exit-code mapping for command and signal outcomes.

use std::process::ExitCode;

/// The outcome category observed by the process edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitKind {
    Success,
    RequestedFailure,
    Usage,
    Internal,
    Signal(u8),
}

/// Maps one typed outcome to its stable process status.
pub(crate) const fn status(kind: ExitKind) -> u8 {
    match kind {
        ExitKind::Success => 0,
        ExitKind::RequestedFailure => 1,
        ExitKind::Usage => 124,
        ExitKind::Internal => 125,
        ExitKind::Signal(signal) => 128_u8.saturating_add(signal),
    }
}

pub(crate) fn code(kind: ExitKind) -> ExitCode {
    ExitCode::from(status(kind))
}

#[cfg(test)]
mod tests {
    use super::{ExitKind, status};

    #[test]
    fn stable_failure_and_signal_statuses() {
        assert_eq!(status(ExitKind::Success), 0);
        assert_eq!(status(ExitKind::RequestedFailure), 1);
        assert_eq!(status(ExitKind::Usage), 124);
        assert_eq!(status(ExitKind::Internal), 125);
        assert_eq!(status(ExitKind::Signal(2)), 130);
        assert_eq!(status(ExitKind::Signal(13)), 141);
        assert_eq!(status(ExitKind::Signal(15)), 143);
    }
}
