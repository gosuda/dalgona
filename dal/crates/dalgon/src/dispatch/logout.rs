use std::{io::Write, path::Path, process::ExitCode};

use dal_provider::{AuthStore, Catalog, CatalogSource, ProviderConfig, ProviderError};

use crate::{Startup, cli, exit, two_lines};

/// Removes one stored provider credential or every credential.
pub(crate) async fn run(args: cli::ProviderArgs, startup: Startup) -> ExitCode {
    let providers: Vec<&str> = match args.provider.as_deref() {
        Some(provider) if super::login::LOGIN_PROVIDERS.contains(&provider) => vec![provider],
        Some(provider) => {
            return two_lines(
                [
                    format!("dalgon: unknown provider \"{provider}\""),
                    crate::cli::texts::LOGIN_PROVIDER_HINT.into(),
                ],
                exit::ExitKind::Usage,
            );
        }
        None => super::login::LOGIN_PROVIDERS.to_vec(),
    };
    let all = args.provider.is_none();
    let path = startup.data_root.join("auth.json");
    let mut removed = Vec::new();
    for provider in providers {
        let mut store = match AuthStore::load(&path) {
            Ok(store) => store,
            Err(error) => return auth_error(&error, &path),
        };
        if store.credential(provider).is_none() {
            continue;
        }
        if let Err(error) = dal_provider::logout(provider, &mut store).await {
            return auth_error(&error, &path);
        }
        removed.push(provider);
    }
    if all {
        let message = if removed.is_empty() {
            crate::cli::texts::LOGOUT_NONE
        } else {
            crate::cli::texts::LOGOUT_ALL
        };
        let _ = writeln!(std::io::stdout().lock(), "{message}");
        warn_saved_model_provider(&startup, &removed);
        return exit::code(exit::ExitKind::Success);
    }
    let provider = args.provider.as_deref().unwrap_or_default();
    let message = if removed.is_empty() {
        crate::cli::texts::LOGOUT_NONE.to_owned()
    } else {
        format!("Removed credentials for {provider}.")
    };
    let _ = writeln!(std::io::stdout().lock(), "{message}");
    warn_saved_model_provider(&startup, &removed);
    exit::code(exit::ExitKind::Success)
}

fn warn_saved_model_provider(startup: &Startup, removed: &[&str]) {
    if removed.is_empty() {
        return;
    }
    let Ok(config) = ProviderConfig::from_config(&startup.config) else {
        return;
    };
    let Some(reference) = startup.config.model() else {
        return;
    };
    let sources = config
        .providers
        .iter()
        .cloned()
        .map(|provider| (provider, CatalogSource::Typed))
        .collect();
    let aliases = startup
        .config
        .aliases()
        .iter()
        .map(|(name, target)| (name.clone(), target.clone()))
        .collect::<Vec<_>>();
    let catalog = Catalog::with_sources(sources, Vec::new());
    let Ok(model) = dal_provider::resolve(&catalog, &aliases, reference) else {
        return;
    };
    if !removed.contains(&model.provider.as_ref()) {
        return;
    }
    let message = crate::cli::texts::saved_model_provider_warning(&model.entry.id, &model.provider);
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}

fn auth_error(error: &ProviderError, path: &Path) -> ExitCode {
    let (what, hint) = match &error {
        ProviderError::AuthFileInvalid { message, .. } => (
            format!("dalgon: auth.json is not valid JSON: {message}"),
            crate::cli::texts::AUTH_INVALID_HINT.to_owned(),
        ),
        ProviderError::AuthFilePerms { path } => (
            format!("dalgon: {error}"),
            format!("Run chmod 600 {}.", path.display()),
        ),
        ProviderError::AuthFileSymlink { path } => (
            format!("dalgon: {error}"),
            format!("Replace {} with a regular file.", path.display()),
        ),
        _ => (
            format!("dalgon: cannot update auth.json: {error}"),
            format!("Check the permissions of {} and try again.", path.display()),
        ),
    };
    two_lines([what, hint], exit::ExitKind::RequestedFailure)
}
