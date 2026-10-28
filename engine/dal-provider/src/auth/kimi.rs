//! The Kimi Code client identity.
//!
//! `api.kimi.com/coding` identifies its clients by a product `User-Agent` plus
//! an `X-Msh-*` device header set, and the official client sends that set on
//! every OAuth and managed-API request of a subscription session. The key
//! sign-in authenticates with a platform key rather than a client session and
//! stays header-free: only a Kimi Code OAuth credential carries the set.
//!
//! Three contract details are not visible from the code: every value must be
//! printable ASCII (the endpoint sits behind a CDN that answers 520 on raw
//! non-ASCII header bytes), the device id must be stable per install, and the
//! platform string names a client family rather than the host product.
//!
//! Nothing here reads the process environment or the platform: the values
//! come from the [`ProviderIdentity`] and the [`EnvSnapshot`] the binary
//! captured at startup, and the device id from one private file next to
//! `auth.json`. Persisting the id is best effort: an unwritable data
//! directory degrades to an id that lives for the process, never to a failed
//! sign-in or request.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::{EnvSnapshot, ProviderError, ProviderIdentity};

/// The `User-Agent` product of every Kimi Code OAuth and subscription
/// request. The official CLI sends `<product>/<version>` and names its
/// client family with [`CLIENT_PLATFORM`]; both come from
/// `apps/kimi-code/src/constant/app.ts` of the pinned `MoonshotAI/kimi-code`
/// source, and the version this client sends is its own build's.
pub(crate) const USER_AGENT_PRODUCT: &str = "kimi-code-cli";

/// The client family the endpoint expects, not the host product.
const CLIENT_PLATFORM: &str = "kimi_code_cli";

/// The file beside `auth.json` that keeps the per-install device id. The
/// official CLI names its file `device_id` under its home directory; this
/// crate keeps one beside `auth.json`, where the store's other secrets
/// live, under a name no CLI install collides with.
pub(crate) const DEVICE_ID_FILE: &str = "kimi-device-id";

/// The value of a header whose source is empty or not printable ASCII.
const UNKNOWN: &str = "unknown";

/// The captured variables that name this host, in order. The library never
/// asks the platform for a host name; a host that exports none sends
/// [`UNKNOWN`], the pinned fallback for an empty one.
const HOST_NAME_VARIABLES: [&str; 2] = ["HOSTNAME", "COMPUTERNAME"];

/// The `X-Msh-*` headers and the `User-Agent` of one install, derived once
/// from the captured identity. The HTTP layers of the library own no Kimi
/// header: a request that names the client takes [`ClientIdentity::user_agent`]
/// and [`ClientIdentity::headers`] verbatim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClientIdentity {
    user_agent: String,
    headers: Vec<(&'static str, String)>,
}

impl ClientIdentity {
    /// The identity of the host `identity` names, with the install's
    /// `device_id` (see [`device_id`]) and the host name `env` captured.
    /// The header names and values are the official CLI's
    /// (`packages/oauth/src/identity.ts` of the pinned `MoonshotAI/kimi-code`
    /// source), with this build's version in `X-Msh-Version` and the
    /// `User-Agent`.
    pub(crate) fn new(identity: &ProviderIdentity, env: &EnvSnapshot, device_id: &str) -> Self {
        let host = HOST_NAME_VARIABLES
            .into_iter()
            .find_map(|name| env.get(name).filter(|value| !value.trim().is_empty()))
            .unwrap_or_default();
        let version = printable(&identity.version);
        let user_agent = format!("{USER_AGENT_PRODUCT}/{version}");
        Self {
            user_agent,
            headers: vec![
                ("X-Msh-Platform", String::from(CLIENT_PLATFORM)),
                ("X-Msh-Version", version),
                ("X-Msh-Device-Name", printable(host)),
                ("X-Msh-Device-Model", printable(&device_model(identity))),
                ("X-Msh-Os-Version", printable(&identity.os_version)),
                ("X-Msh-Device-Id", printable(device_id)),
            ],
        }
    }

    /// The `User-Agent` of every Kimi Code OAuth and subscription request.
    pub(crate) fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// The headers, in the order a request sends them. Header names are
    /// case-insensitive on the wire (RFC 9110, section 5.1); they keep the
    /// official spelling here.
    pub(crate) fn headers(&self) -> &[(&'static str, String)] {
        &self.headers
    }
}

/// `value` with every byte outside printable ASCII removed and the ends
/// trimmed, or [`UNKNOWN`] when nothing is left.
fn printable(value: &str) -> String {
    let kept: String = value
        .chars()
        .filter(|character| (' '..='~').contains(character))
        .collect();
    let trimmed = kept.trim();
    if trimmed.is_empty() {
        String::from(UNKNOWN)
    } else {
        trimmed.to_owned()
    }
}

/// `<platform label> <os version> <arch>`, each part present only when the
/// captured identity names it.
fn device_model(identity: &ProviderIdentity) -> String {
    let label = match &*identity.os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    [label, &*identity.os_version, &*identity.arch]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The one [`ClientIdentity`] of a provider set, derived on first use.
///
/// Everything that names the client after the sign-in (the refresher and
/// every request) shares one source, so they send one device id. The first
/// use reads the id the sign-in wrote beside `auth.json` on the blocking
/// pool; building a provider set reads and writes nothing. A source nobody
/// asks for costs nothing.
#[derive(Debug)]
pub(crate) struct ClientIdentitySource {
    identity: ProviderIdentity,
    env: EnvSnapshot,
    dir: PathBuf,
    derived: OnceCell<Arc<ClientIdentity>>,
}

impl ClientIdentitySource {
    /// A source over the host platform, the captured environment, and the
    /// directory `auth.json` lives in.
    pub(crate) fn new(identity: ProviderIdentity, env: EnvSnapshot, dir: PathBuf) -> Self {
        Self {
            identity,
            env,
            dir,
            derived: OnceCell::new(),
        }
    }

    /// The identity, derived once; every caller gets the same value.
    ///
    /// # Errors
    ///
    /// [`ProviderError::AuthWrite`] when the blocking task fails.
    pub(crate) async fn get(&self) -> Result<Arc<ClientIdentity>, ProviderError> {
        self.derived
            .get_or_try_init(|| async {
                let (identity, env, dir) =
                    (self.identity.clone(), self.env.clone(), self.dir.clone());
                crate::auth::refresh::blocking(move || {
                    let device_id = installed_device_id(&dir);
                    Ok(Arc::new(ClientIdentity::new(&identity, &env, &device_id)))
                })
                .await
            })
            .await
            .cloned()
    }
}

/// The trimmed content of `<dir>/kimi-device-id`, when the file holds one.
fn read_device_id(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(DEVICE_ID_FILE))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// A fresh 32-hex device id.
fn new_device_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// The install's device id for a sign-in: the trimmed content of
/// `<dir>/kimi-device-id`, or a fresh 32-hex id written there on first use
/// (owner-only, never replacing a file another process published first). The
/// sign-in is the first request that names the client, so it is where the id
/// comes to exist. Blocking file work: callers on the async runtime run it
/// on the blocking pool.
pub(crate) fn device_id(dir: &Path) -> String {
    if let Some(existing) = read_device_id(dir) {
        return existing;
    }
    let minted = new_device_id();
    let published = dal_store::create_private_dir_all(dir).is_ok()
        && dal_store::write_atomic_new(
            &path_of(dir),
            format!("{minted}\n").as_bytes(),
            dal_store::FileMode::Mode0600,
        )
        .is_ok();
    if published {
        return minted;
    }
    // Another process may have published first; otherwise the id lives for
    // this process only.
    read_device_id(dir).unwrap_or(minted)
}

/// The install's device id for a request or a renewal: the id a sign-in
/// already wrote, or, when there is none (no Kimi Code sign-in happened
/// here, or the file was removed), a process-local id. It writes nothing, so
/// building a provider set never touches the data directory.
pub(crate) fn installed_device_id(dir: &Path) -> String {
    read_device_id(dir).unwrap_or_else(new_device_id)
}

/// The device-id file under `dir`.
fn path_of(dir: &Path) -> std::path::PathBuf {
    dir.join(DEVICE_ID_FILE)
}

#[cfg(test)]
mod tests;
