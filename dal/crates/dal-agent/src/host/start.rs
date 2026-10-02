//! Host construction from an explicit product, config, and env snapshot.

use std::sync::Arc;

use dal_core::Config;
use dal_provider::{EnvSnapshot, ProviderConfig, ProviderIdentity, ProviderSet};

use super::{Env, Host, HostShared, HostState, Product};
use crate::admission::{Admission, Limits};
use crate::error::HostError;
use crate::ext::generation::{Generation, ValidatedExtensions};

impl Host {
    /// Builds the host state from the product factory output.
    ///
    /// The first extension generation validates before anything publishes;
    /// a conflict fails startup with its registration text.
    ///
    /// # Errors
    /// Returns [`HostError::Config`] when extension registrations or provider
    /// configuration cannot be validated or initialized.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the public startup API is awaited by callers across the workspace; startup validation stays async for one composition shape"
    )]
    pub async fn start(product: Product, config: Config, env: Env) -> Result<Self, HostError> {
        let attaches: Vec<crate::ext::Attach> = product
            .extensions
            .iter()
            .flat_map(crate::ext::Extension::take_attaches)
            .collect();
        let validated =
            ValidatedExtensions::validate(product.extensions, None).map_err(|error| {
                HostError::Config {
                    message: error.to_string().into(),
                }
            })?;
        let generation = Generation::build(validated);
        let commands: Arc<[dal_core::CommandSpec]> = generation
            .commands
            .entries()
            .iter()
            .filter_map(|entry| {
                generation
                    .command(&entry.name)
                    .map(|(spec, _)| spec.clone())
            })
            .collect();
        let (generation_tx, _) = tokio::sync::watch::channel(Arc::new(generation));
        let plugin_base = HostShared::base_names(&generation_tx.borrow());
        let providers = provider_set(&config, &env, &product.data_root)?;
        let shared = Arc::new(HostShared {
            config,
            env: Arc::new(env),
            product_name: product.name,
            data_root: product.data_root,
            admission: Admission::new(Limits::default(), fd_soft_limit()),
            interpreters: Arc::new(crate::admission::Interpreters::new()),
            providers,
            commands,
            generation: generation_tx,
            catalog: std::sync::RwLock::new(None),
            plugin_base,
        });
        let host = Self {
            state: Arc::new(HostState {
                sessions: std::sync::Mutex::default(),
                subscribers: std::sync::Mutex::default(),
                shared,
                attached: std::sync::Mutex::default(),
            }),
        };
        {
            let mut tasks = host
                .state
                .attached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for attach in attaches {
                tasks.spawn(attach(host.clone()));
            }
        }
        Ok(host)
    }
}

/// Reads the process fd soft limit without touching other process state.
#[cfg(unix)]
fn fd_soft_limit() -> u64 {
    rustix::process::getrlimit(rustix::process::Resource::Nofile)
        .current
        .unwrap_or(1024)
}

/// Returns the conservative fallback: Windows has no `getrlimit`, so the
/// admission budget uses the same floor the unix reader applies on error.
#[cfg(windows)]
fn fd_soft_limit() -> u64 {
    1024
}

/// Builds the provider set from the typed provider configuration.
fn provider_set(
    config: &Config,
    env: &Env,
    data_root: &std::path::Path,
) -> Result<ProviderSet, HostError> {
    let provider_config =
        ProviderConfig::from_config(config).map_err(|error| HostError::Config {
            message: error.to_string().into(),
        })?;
    let identity = ProviderIdentity {
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        os_version: String::new().into(),
        arch: std::env::consts::ARCH.into(),
    };
    ProviderSet::new(
        &provider_config,
        identity,
        EnvSnapshot::from_vars(&env.vars),
        data_root,
        &data_root.join("cache"),
    )
    .map_err(|error| HostError::Config {
        message: error.to_string().into(),
    })
}
