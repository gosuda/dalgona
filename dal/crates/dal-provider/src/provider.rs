//! Provider request orchestration and the public family-neutral provider API.

mod api;
mod config;
mod debug;
mod def;
mod request;
mod set;
mod shape;
mod transport;

pub use api::{Http, Provider};
pub use config::{
    AuthStyle, ProviderConfig, ProviderConfigError, ProviderEntry, ScriptedSelection, Transport,
};
pub use def::{
    Completion, Device, Hook, KeySpec, OAuthSpec, PROVIDERS, ProviderDef, QueryValue, Redirect,
    find,
};
pub(crate) use def::{OPENAI, OPENAI_CODEX};
pub use set::{ProviderIdentity, ProviderSet};
pub use shape::Shape;

#[cfg(test)]
mod tests;
