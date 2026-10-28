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
    /// Awaiting is the only composition shape: callers `.await` this like any
    /// async constructor while the body stays synchronous.
    pub async fn start(product: Product, config: Config, env: Env) -> Result<Self, HostError> {
        std::future::ready(Self::start_with_provider_setup(
            product,
            config,
            env,
            |_| Ok(()),
        ))
        .await
    }

    /// Internal startup seam that allows configuring the [`ProviderSet`] before
    /// login-site attachment or host sharing.
    pub(crate) fn start_with_provider_setup(
        product: Product,
        config: Config,
        env: Env,
        setup: impl FnOnce(&mut ProviderSet) -> Result<(), HostError>,
    ) -> Result<Self, HostError> {
        raise_fd_soft_limit();
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
        let snapshot = EnvSnapshot::from_vars(&env.vars).with_cwd(env.cwd.clone());
        let login_site = dal_provider::LoginSite::new(
            product.data_root.join("auth.json"),
            product.data_root.join("cache"),
            &provider_identity(),
            &snapshot,
        )
        .map_err(HostError::Provider)?;
        let mut providers = provider_set(&config, snapshot, &product.data_root)?;
        setup(&mut providers)?;
        // The set exists before attach so the site clones the same grant cache
        // handle; there is exactly one allocation.
        let login_site = login_site.with_provider_set(&providers);
        // The census workers the snapshot coordinator spawns are tracked
        // by this owner and drained by host shutdown.
        let snapshot_tasks = crate::session::tasks::SessionTasks::new();
        let shared = Arc::new(HostShared {
            config: std::sync::RwLock::new(config),
            env: Arc::new(env),
            product_name: product.name,
            data_root: product.data_root,
            admission: Admission::new(Limits::default(), fd_soft_limit()),
            snapshots: crate::proc::SnapshotCoordinator::host(
                Limits::default().processes,
                snapshot_tasks,
            ),
            interpreters: Arc::new(crate::admission::Interpreters::new()),
            providers,
            commands,
            generation: generation_tx,
            catalog: std::sync::RwLock::new(None),
            login_site: std::sync::RwLock::new(login_site),
            config_path: std::sync::RwLock::new(None),
            config_write: tokio::sync::Mutex::new(()),
            plugin_base,
        });
        let host = Self {
            state: Arc::new(HostState {
                sessions: std::sync::Mutex::default(),
                name_claims: std::sync::Mutex::default(),
                subscribers: std::sync::Mutex::default(),
                logins: std::sync::Mutex::default(),
                next_login: std::sync::atomic::AtomicU64::new(1),
                shared,
                attached: std::sync::Mutex::default(),
                session_teardowns: crate::session::tasks::SessionTasks::new(),
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

/// The fd ceiling a host lifts its inherited soft limit toward when the
/// environment grants a higher hard limit.
#[cfg(unix)]
const FD_TARGET: u64 = 16_384;

/// Lifts the process fd soft limit before admission is sized.
///
/// Every live session holds a lock and a journal descriptor, every job needs
/// pipes, and transports hold sockets, so an inherited default like macOS's
/// 256 starves the whole host at once; opens then fail EMFILE in paths the
/// fd admission gate never measured. The soft limit can always rise to the
/// hard ceiling without privilege, so the host claims the headroom its
/// environment already grants. Best effort: a denied setrlimit leaves the
/// inherited limit, which the budget still defends.
#[cfg(unix)]
fn raise_fd_soft_limit() {
    let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    let soft = limit.current.unwrap_or(FD_TARGET);
    // The kernel can cap descriptors below the hard limit (macOS
    // kern.maxfilesperproc), so halve down from the target until a setrlimit
    // lands instead of failing on the first unreachable rung.
    let mut target = limit.maximum.unwrap_or(FD_TARGET).min(FD_TARGET);
    while target > soft {
        if rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(target),
                maximum: None,
            },
        )
        .is_ok()
        {
            break;
        }
        target /= 2;
    }
    let after = rustix::process::getrlimit(rustix::process::Resource::Nofile)
        .current
        .unwrap_or(soft);
    if after != soft {
        tracing::debug!(target: "dalgon.agent", soft, after, "fd soft limit raised");
    }
}

/// Windows has no `getrlimit`; the process edge does no fd lifting there.
#[cfg(windows)]
fn raise_fd_soft_limit() {}

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

#[cfg(all(test, unix))]
mod tests {
    /// The lift must leave the soft limit at least at the hard-capped target
    /// so session locks, journals, pipes, and sockets fit: the macOS default
    /// of 256 starved ~200 concurrent sessions of descriptors entirely.
    #[test]
    fn raise_fd_soft_limit_reaches_the_kernel_ceiling() {
        let before = rustix::process::getrlimit(rustix::process::Resource::Nofile)
            .current
            .unwrap_or(0);
        super::raise_fd_soft_limit();
        let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        let hard = limit.maximum.unwrap_or(u64::MAX);
        let soft = limit.current.unwrap_or(0);
        assert!(
            soft >= before,
            "the lift never lowers the fd soft limit (before={before}, soft={soft})"
        );
        // The ladder bottoms out at 1024: any saner floor must be at least
        // that, and a kernel that hard-caps below it still leaves the cap.
        let floor = hard.min(1024);
        assert!(
            soft >= floor,
            "fd soft limit {soft} stayed below the reachable floor {floor} (hard={hard})"
        );
    }
}

/// The host platform the provider crate renders its `User-Agent` and its
/// Cloud Code Assist project discovery from. The process edge captures it
/// here, once per use site, so the library never reads the platform itself;
/// the provider set and the login site therefore see the same values.
fn provider_identity() -> ProviderIdentity {
    ProviderIdentity {
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        os_version: String::new().into(),
        arch: std::env::consts::ARCH.into(),
    }
}

/// Builds the provider set from the typed provider configuration.
///
/// `snapshot` is the host's captured environment, built once in
/// [`Host::start`] and shared with the login site so both read the same
/// values.
fn provider_set(
    config: &Config,
    snapshot: EnvSnapshot,
    data_root: &std::path::Path,
) -> Result<ProviderSet, HostError> {
    let provider_config =
        ProviderConfig::from_config(config).map_err(|error| HostError::Config {
            message: error.to_string().into(),
        })?;
    ProviderSet::new(
        &provider_config,
        provider_identity(),
        snapshot,
        data_root,
        &data_root.join("cache"),
    )
    .map_err(|error| HostError::Config {
        message: error.to_string().into(),
    })
}
