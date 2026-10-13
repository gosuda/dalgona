//! Hashline Enhanced profile: snapshot-bound, observation-required.

use super::super::ir::{Edit, ParseError};

/// Parses Enhanced payloads; syntax matches Light, coverage differs at prepare.
pub(crate) fn parse(input: &str, symbols: bool) -> Result<Vec<Edit>, ParseError> {
    super::hashline_light::parse_profile(input, symbols)
}
