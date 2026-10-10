use std::{io::Write, path::Path, process::ExitCode};

use dal_agent::{HostError, Product};
use dal_provider::{Catalog, CatalogSource, ProviderConfig, ProviderError};

use super::login::{AuthHost, shutdown_auth_host, start_auth_host};
use crate::{Config, Startup, cli, exit, two_lines};

/// Removes one stored provider credential or every credential through the
/// shared [`dal_agent::Host`] operation, the same path the terminal and RPC
/// front ends use.
pub(crate) async fn run(args: cli::ProviderArgs, startup: Startup, product: Product) -> ExitCode {
    if let Some(provider) = args.provider.as_deref()
        && !super::login::is_login_provider(provider)
    {
        return super::login::unknown_provider(provider);
    }
    let all = args.provider.is_none();
    let config = startup.config.clone();
    let AuthHost { host, auth_path, .. } = match start_auth_host(startup, product).await {
        Ok(auth) => auth,
        Err(code) => return code,
    };
    let path = auth_path;
    let mut removed: Vec<Box<str>> = Vec::new();
    for provider in args
        .provider
        .as_deref()
        .map_or_else(|| super::login::provider_ids().collect(), |one| vec![one])
    {
        match host.logout(Some(provider)).await {
            Ok(done) => removed.extend(done),
            Err(error) => {
                shutdown_auth_host(host).await;
                return logout_error(&error, &path);
            }
        }
    }
    shutdown_auth_host(host).await;
    let removed: Vec<&str> = removed.iter().map(AsRef::as_ref).collect();
    if all {
        let message = if removed.is_empty() {
            crate::cli::texts::LOGOUT_NONE
        } else {
            crate::cli::texts::LOGOUT_ALL
        };
        let _ = writeln!(std::io::stdout().lock(), "{message}");
        warn_saved_model_provider(&config, &removed);
        return exit::code(exit::ExitKind::Success);
    }
    let provider = args.provider.as_deref().unwrap_or_default();
    let message = if removed.is_empty() {
        crate::cli::texts::LOGOUT_NONE.to_owned()
    } else {
        format!("Removed credentials for {provider}.")
    };
    let _ = writeln!(std::io::stdout().lock(), "{message}");
    warn_saved_model_provider(&config, &removed);
    exit::code(exit::ExitKind::Success)
}

fn warn_saved_model_provider(config: &Config, removed: &[&str]) {
    if removed.is_empty() {
        return;
    }
    let Ok(provider_config) = ProviderConfig::from_config(config) else {
        return;
    };
    let Some(reference) = config.model() else {
        return;
    };
    let sources = provider_config
        .providers
        .iter()
        .cloned()
        .map(|provider| (provider, CatalogSource::Typed))
        .collect();
    let aliases = config
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

fn logout_error(error: &HostError, path: &Path) -> ExitCode {
    let HostError::Provider(error) = error else {
        return super::login::host_auth_error("logout", error, path);
    };
    auth_error(error, path)
}

fn auth_error(error: &ProviderError, path: &Path) -> ExitCode {
    let (what, hint) = match error {
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
