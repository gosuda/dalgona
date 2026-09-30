//! Provider request orchestration and the public family-neutral provider API.

mod api;
mod config;
mod debug;
mod request;
mod set;
mod transport;

pub use api::{Http, Provider};
pub use config::{
    AuthStyle, ProviderConfig, ProviderConfigError, ProviderEntry, ScriptedSelection, Transport,
};
pub use set::{ProviderIdentity, ProviderSet};

#[cfg(test)]
mod tests;
