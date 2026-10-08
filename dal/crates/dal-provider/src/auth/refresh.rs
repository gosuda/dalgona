//! Serialized OAuth token refresh over `auth.json`.
//!
//! One [`Refresher`] owns one `auth.json` path. Inside a process each OAuth
//! credential key (`anthropic`, `openai-codex`) has one async mutex; across
//! processes an exclusive advisory lock on the sibling `auth.json.lock`
//! (`std::fs::File::try_lock`, polled without blocking the runtime)
//! serializes every refresher of the same file. Holding both, the refresher
//! reloads `auth.json` and decides from the reloaded entry:
//!
//! - the stored access token differs from the one the caller holds: another
//!   caller or process already refreshed, so the stored credential is used and
//!   no request is sent;
//! - [`RefreshReason::Expiring`] and the stored token has more than
//!   [`PROACTIVE_WINDOW_SECS`] left (or no expiry at all): the stored
//!   credential is used and no request is sent;
//! - otherwise one refresh request goes to the token endpoint, retried once
//!   after [`RETRY_DELAY`] on a transient failure and never after a rejected
//!   refresh token, and the new tokens are committed by the atomic rename of
//!   [`AuthStore::store`].
//!
//! The commit is the last step. A caller can drop its future while it waits
//! for either lock; the per-key slot keeps an in-flight task so a later caller
//! awaits that same refresh. The task keeps the auth file lock through the
//! bounded exchange and atomic commit. The commit runs on the blocking pool
//! and all file I/O runs there. The file is never half-written, and a rotated
//! refresh token is not discarded by cancellation.
//! Nothing here reads the environment or keeps process-global state, and no
//! error text or log line carries a token.
//!
//! [`AuthStore::store`]: crate::auth::credential::AuthStore::store

use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{error::ProviderError, http::OAUTH_TIMEOUT};

mod endpoints;
mod engine;

pub use endpoints::{OAuthProvider, RefreshReason, TokenEndpoints};
pub use engine::Refresher;

/// Seconds before `expires_at` at which a proactive refresh starts.
pub const PROACTIVE_WINDOW_SECS: i64 = 300;

/// Wait between the first transient refresh failure and the single retry.
pub const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Poll interval while another process holds `auth.json.lock`.
pub(crate) const LOCK_POLL: Duration = Duration::from_millis(20);

/// Longest wait for `auth.json.lock`: one full refresh by the holder (two
/// attempts bounded by [`OAUTH_TIMEOUT`] plus the retry delay) with slack for
/// its file writes.
pub(crate) const LOCK_WAIT: Duration =
    Duration::from_secs(2 * OAUTH_TIMEOUT.as_secs() + RETRY_DELAY.as_secs() + 5);

/// Token endpoint paths under a replay or test base, matching the paths of
/// `CODEX_TOKEN_URL` and `CLAUDE_TOKEN_URL`.
pub(crate) const ANTHROPIC_TOKEN_PATH: &str = "v1/oauth/token";
pub(crate) const CODEX_TOKEN_PATH: &str = "oauth/token";

/// Token-endpoint error codes that mean the refresh token is dead; the user
/// must sign in again and no retry can help.
pub(crate) const PERMANENT_CODES: [&str; 4] = [
    "invalid_grant",
    "refresh_token_expired",
    "refresh_token_reused",
    "refresh_token_invalidated",
];

/// Takes the exclusive advisory lock on the `auth.json.lock` sibling of
/// `auth_path`, polling without blocking the runtime. Dropping the returned
/// file releases the lock; the lock file stays. Every writer of `auth.json`
/// (refresh, login, logout) holds this lock across its reload, `set`, and
/// `store`, so none overwrites another's entry.
pub(crate) async fn lock_auth_file(auth_path: &Path) -> Result<File, ProviderError> {
    let lock_path = lock_path(auth_path)?;
    let open_path = lock_path.clone();
    let file = blocking(move || {
        open_lock_file(&open_path).map_err(|error| ProviderError::AuthWrite {
            reason: format!("could not open {}: {error}", open_path.display()),
        })
    })
    .await?;
    let deadline = tokio::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(TryLockError::WouldBlock) => {
                return Err(ProviderError::AuthWrite {
                    reason: format!(
                        "{} stayed locked by another process for {} s",
                        lock_path.display(),
                        LOCK_WAIT.as_secs()
                    ),
                });
            }
            Err(TryLockError::Error(error)) => {
                return Err(ProviderError::AuthWrite {
                    reason: format!("could not lock {}: {error}", lock_path.display()),
                });
            }
        }
    }
}

/// Runs file work on the blocking pool; the work runs to its end even when
/// the awaiting future is dropped.
pub(crate) async fn blocking<T, F>(work: F) -> Result<T, ProviderError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ProviderError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ProviderError::AuthWrite {
            reason: format!("the auth.json task failed: {error}"),
        })?
}

fn lock_path(auth_path: &Path) -> Result<PathBuf, ProviderError> {
    let Some(name) = auth_path.file_name() else {
        return Err(ProviderError::AuthWrite {
            reason: format!("{} names no file", auth_path.display()),
        });
    };
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    Ok(auth_path.with_file_name(lock_name))
}

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests;
