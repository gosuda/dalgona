//! Host sign-in operations: login, logout, and the stored-credential list.
//!
//! Every front end (terminal, CLI edge, RPC) signs in and out through these
//! calls. The provider layer owns the flows; the host adds the one terminal
//! [`HostUpdate::LoginFinished`] per login and the typed error mapping.

use dal_provider::{Credential, LoginIo, LoginSite, Method, StoredCredential};

use super::{Host, HostUpdate};
use crate::error::HostError;

/// What a finished sign-in stored, without its secrets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginOutcome {
    /// The provider that was signed in to.
    pub provider: Box<str>,
    /// The method that was used.
    pub method: Method,
    /// The account the sign-in names, when the provider reports one.
    pub account: Option<Box<str>>,
}

impl Host {
    /// Signs in to `provider` with `method` and stores the credential.
    ///
    /// OAuth methods report [`dal_provider::LoginProgress`] on
    /// [`LoginIo::progress`], read one pasted value from [`LoginIo::paste`]
    /// when the flow asks, and stop at [`LoginIo::cancel`]. [`Method::ApiKey`]
    /// reads the key from [`LoginIo::paste`]. The `auth.json` write is the last
    /// step, so a cancelled or failed login leaves the file unchanged.
    ///
    /// Every login that runs to its end publishes exactly one
    /// [`HostUpdate::LoginFinished`], ready or failed; cancellation is a
    /// failure and never reports success. Dropping this future before it ends
    /// publishes nothing: cancel through the token instead.
    ///
    /// # Errors
    /// Returns [`HostError::Provider`] with the typed sign-in failure:
    /// an unsupported method, cancellation, the 15-minute deadline, a callback
    /// or token-exchange failure, or an `auth.json` error.
    pub async fn login(
        &self,
        provider: &str,
        method: Method,
        io: LoginIo,
    ) -> Result<LoginOutcome, HostError> {
        let site = self.login_site();
        let result = dal_provider::login(provider, method, io, &site).await;
        let (ready, detail) = match &result {
            Ok(_) => (true, None),
            Err(error) => (false, Some(Box::<str>::from(error.to_string()))),
        };
        self.publish(&HostUpdate::LoginFinished {
            provider: provider.into(),
            ready,
            detail,
        });
        let credential = result.map_err(HostError::Provider)?;
        Ok(LoginOutcome {
            provider: provider.into(),
            method,
            account: match credential {
                Credential::OAuth(oauth) => oauth.account_id.map(Into::into),
                Credential::ApiKey { .. } | Credential::None => None,
            },
        })
    }

    /// Removes the stored credential of `provider`, or of every provider when
    /// `None`, and returns the providers that had one.
    ///
    /// A Codex OAuth credential is revoked on a best-effort basis first; a
    /// provider with no stored credential makes no request and is not listed.
    /// Environment variables are untouched.
    ///
    /// # Errors
    /// Returns [`HostError::Provider`] when the durable removal fails.
    pub async fn logout(&self, provider: Option<&str>) -> Result<Vec<Box<str>>, HostError> {
        let site = self.login_site();
        dal_provider::sign_out(provider, &site)
            .await
            .map_err(HostError::Provider)
    }

    /// Lists the credentials stored in `auth.json`, without their secrets.
    ///
    /// # Errors
    /// Returns [`HostError::Provider`] when `auth.json` cannot be read.
    pub async fn stored_credentials(&self) -> Result<Vec<StoredCredential>, HostError> {
        let site = self.login_site();
        tokio::task::spawn_blocking(move || dal_provider::stored_credentials(&site))
            .await
            .map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })?
            .map_err(HostError::Provider)
    }

    /// Replaces the pinned production sign-in endpoints with literal-loopback
    /// ones, so a test can run every flow against a local server.
    ///
    /// # Errors
    /// Returns [`HostError::Provider`] when an endpoint leaves the allowed
    /// origins.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_login_endpoints(
        &self,
        endpoints: dal_provider::LoginEndpoints,
    ) -> Result<(), HostError> {
        let site = self
            .login_site()
            .with_endpoints(endpoints)
            .map_err(HostError::Provider)?;
        *self
            .state
            .shared
            .login_site
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = site;
        Ok(())
    }

    fn login_site(&self) -> LoginSite {
        self.state
            .shared
            .login_site
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests;
