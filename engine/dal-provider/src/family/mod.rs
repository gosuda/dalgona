//! Provider-specific request bodies and stream decoders.

pub(crate) mod anthropic;
pub(crate) mod azure;
pub(crate) mod bedrock;
pub(crate) mod chat;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod cursor;
pub(crate) mod devin;
pub(crate) mod droid;
pub(crate) mod gitlab;
pub(crate) mod google;
pub(crate) mod mistral;
pub(crate) mod ollama;
pub(crate) mod pi_messages;
mod process;
pub(crate) mod responses;
pub(crate) mod schema;
