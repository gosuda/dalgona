use std::{io::Write, process::ExitCode};

use dal_provider::{
    Catalog, CatalogEntry, EnvSnapshot, Listing, ProviderConfig, ProviderIdentity, ProviderSet,
    compiled_price, price_source,
};
use serde::Serialize;

use crate::{Startup, cli, exit, two_lines};

#[derive(Serialize)]
struct ModelOutput<'a> {
    provider: &'a str,
    id: &'a str,
    context: Option<u32>,
}

/// Fetches and lists the configured catalog, retaining cached rows on failure.
pub(crate) async fn run(args: cli::ModelsArgs, startup: Startup) -> ExitCode {
    let provider_config = match ProviderConfig::from_config(&startup.config) {
        Ok(config) => config,
        Err(error) => return config_error(&error.to_string()),
    };
    let providers = match ProviderSet::new(
        &provider_config,
        ProviderIdentity {
            version: env!("CARGO_PKG_VERSION").into(),
            os: std::env::consts::OS.into(),
            os_version: String::new().into(),
            arch: std::env::consts::ARCH.into(),
        },
        EnvSnapshot::from_vars(&startup.vars),
        &startup.data_root,
        &startup.data_root.join("cache"),
    ) {
        Ok(providers) => providers,
        Err(error) => return config_error(&error.to_string()),
    };
    let mut sources = Vec::with_capacity(provider_config.providers.len());
    let mut entries = Vec::new();
    let mut fetch_error: Option<String> = None;
    for provider in &provider_config.providers {
        let ready = match providers.credential(&provider.id) {
            Ok(_) => true,
            Err(dal_provider::ProviderError::NoCredentials { .. }) => false,
            Err(error) => {
                return super::login::provider_error(
                    "models",
                    error,
                    &startup.data_root.join("auth.json"),
                );
            }
        };
        let loaded = match providers.load_models(&provider.id).await {
            Ok(loaded) => loaded,
            Err(
                error @ (dal_provider::ProviderError::AuthFileInvalid { .. }
                | dal_provider::ProviderError::AuthFilePerms { .. }
                | dal_provider::ProviderError::AuthFileSymlink { .. }
                | dal_provider::ProviderError::AuthWrite { .. }),
            ) => {
                return super::login::provider_error(
                    "models",
                    error,
                    &startup.data_root.join("auth.json"),
                );
            }
            Err(error) => return catalog_error(&error.to_string()),
        };
        if ready && let Some(error) = loaded.live_error {
            fetch_error.get_or_insert(error.to_string());
        }
        sources.push((provider.clone(), loaded.source));
        entries.extend(loaded.entries);
    }
    let catalog = Catalog::with_sources(sources, entries);
    let rows = listed_rows(&catalog, args.pattern.as_deref());
    let result = if args.json {
        print_json(&rows)
    } else {
        print_table(&rows)
    };
    if let Err(error) = result {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return exit::code(exit::ExitKind::Signal(13));
        }
        return internal_models_error(&error.to_string(), &startup.data_root);
    }
    match fetch_error {
        Some(error) => catalog_error(&error),
        None => exit::code(exit::ExitKind::Success),
    }
}

fn listed_rows<'a>(catalog: &'a Catalog, pattern: Option<&str>) -> Vec<&'a CatalogEntry> {
    let mut rows: Vec<_> = catalog
        .entries()
        .iter()
        .filter(|entry| entry.listing == Listing::Listed)
        .filter(|entry| {
            pattern.is_none_or(|pattern| {
                entry.provider.contains(pattern) || entry.id.contains(pattern)
            })
        })
        .collect();
    rows.sort_by(|left, right| {
        left.provider
            .as_bytes()
            .cmp(right.provider.as_bytes())
            .then_with(|| left.id.as_bytes().cmp(right.id.as_bytes()))
    });
    rows
}

fn print_json(rows: &[&CatalogEntry]) -> std::io::Result<()> {
    let output: Vec<_> = rows
        .iter()
        .map(|entry| ModelOutput {
            provider: &entry.provider,
            id: &entry.id,
            context: entry.context_window,
        })
        .collect();
    let mut stdout = std::io::stdout().lock();
    let line = sonic_rs::to_string(&output).map_err(std::io::Error::other)?;
    writeln!(stdout, "{line}")
}

fn print_table(rows: &[&CatalogEntry]) -> std::io::Result<()> {
    let provider_width = rows
        .iter()
        .map(|entry| entry.provider.len())
        .max()
        .unwrap_or(8)
        .max(8);
    let id_width = rows
        .iter()
        .map(|entry| entry.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let context_width = rows
        .iter()
        .map(|entry| context_text(entry).len())
        .max()
        .unwrap_or(7)
        .max(7);
    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "{:<provider_width$}  {:<id_width$}  {:<context_width$}  PRICE",
        "PROVIDER", "ID", "CONTEXT"
    )?;
    for entry in rows {
        let context = context_text(entry);
        let price = model_price(entry).map_or_else(|| "-".to_owned(), format_price);
        writeln!(
            stdout,
            "{:<provider_width$}  {:<id_width$}  {:<context_width$}  {price}",
            entry.provider, entry.id, context
        )?;
    }
    let (source, _, date) = price_source();
    writeln!(stdout, "Prices: {source}, fetched {date}.")
}

fn context_text(entry: &CatalogEntry) -> String {
    entry
        .context_window
        .map_or_else(|| "unknown".to_owned(), |value| format!("{value} tokens"))
}

fn model_price(entry: &CatalogEntry) -> Option<dal_core::ModelPrice> {
    let qualified = format!("{}/{}", entry.provider, entry.id);
    compiled_price(&qualified)
}

fn format_price(price: dal_core::ModelPrice) -> String {
    if price.input == 0.0 && price.output == 0.0 {
        return "-".to_owned();
    }
    let input = if price.input == 0.0 {
        "-".to_owned()
    } else {
        format!("${:.3}", price.input)
    };
    let output = if price.output == 0.0 {
        "-".to_owned()
    } else {
        format!("${:.3}", price.output)
    };
    format!("{input}/{output} per M tokens")
}

fn config_error(error: &str) -> ExitCode {
    two_lines(
        [
            format!("dalgon: {error}"),
            crate::cli::texts::CONFIG_FIX_HINT.into(),
        ],
        exit::ExitKind::RequestedFailure,
    )
}

fn catalog_error(error: &str) -> ExitCode {
    two_lines(
        [
            format!("dalgon: could not fetch the model list: {error}"),
            crate::cli::texts::MODELS_FETCH_HINT.into(),
        ],
        exit::ExitKind::RequestedFailure,
    )
}

fn internal_models_error(error: &str, data_root: &std::path::Path) -> ExitCode {
    let log_path = data_root.join("cache").join("dal.log");
    two_lines(
        crate::cli::texts::internal_error_at("models", error, &log_path),
        exit::ExitKind::Internal,
    )
}
