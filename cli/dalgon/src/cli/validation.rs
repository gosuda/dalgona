use super::tree::Cli;

use dal_wire::router::HarnessMode;
/// A root-argument combination the edge must reject before any path work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RootValidation {
    ContinueWithResume,
    ModeWithModeId,
    EmptyName,
    ConnectHeadless,
}

pub(crate) fn is_mode_id(value: &str) -> bool {
    HarnessMode::parse(value).is_some()
}

pub(crate) fn validate_root(cli: &Cli) -> Result<(), RootValidation> {
    if cli.continue_session && cli.resume.is_some() {
        return Err(RootValidation::ContinueWithResume);
    }
    if cli.mode.is_some() && cli.model.as_deref().is_some_and(is_mode_id) {
        return Err(RootValidation::ModeWithModeId);
    }
    if cli.name.as_deref().is_some_and(str::is_empty) {
        return Err(RootValidation::EmptyName);
    }
    if cli.connect.is_some() && (cli.print || cli.json) {
        return Err(RootValidation::ConnectHeadless);
    }
    Ok(())
}
