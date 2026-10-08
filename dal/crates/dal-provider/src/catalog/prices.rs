//! Checked-in price snapshot backing usage fallbacks.
//!
//! The release process owns the snapshot bytes; this module owns the row
//! shape, the `include!` hookup, and the compiled lookups.

use dal_core::{ModelPrice, PriceTier};

pub(crate) struct PriceRow {
    pub(crate) model: &'static str,
    pub(crate) input: Option<f64>,
    pub(crate) cached_input: Option<f64>,
    pub(crate) output: Option<f64>,
    pub(crate) reasoning: Option<f64>,
}

pub(crate) struct TierRow {
    pub(crate) model: &'static str,
    pub(crate) tiers: &'static [PriceTier],
}

include!("../prices_generated.rs");

/// Returns whether the exact provider/model pair is marked for temperature in
/// the compiled models.dev snapshot.
#[must_use]
pub fn compiled_temperature(provider: &str, id: &str) -> bool {
    TEMPERATURE_ROWS.binary_search(&(provider, id)).is_ok()
}

/// Returns the compiled USD-per-million-token rates for an exact provider/model id.
///
/// Missing models have no compiled price. A missing rate in an otherwise
/// priced row contributes zero rather than inventing a different rate.
#[must_use]
pub fn compiled_price(model: &str) -> Option<ModelPrice> {
    let index = PRICE_ROWS
        .binary_search_by_key(&model, |row| row.model)
        .ok()?;
    let row = &PRICE_ROWS[index];
    let tiers = PRICE_TIER_ROWS
        .binary_search_by_key(&model, |row| row.model)
        .ok()
        .map_or_else(Box::<[PriceTier]>::default, |index| {
            PRICE_TIER_ROWS[index].tiers.to_vec().into_boxed_slice()
        });
    Some(ModelPrice {
        input: row.input.unwrap_or(0.0),
        cached_input: row.cached_input.unwrap_or(0.0),
        output: row.output.unwrap_or(0.0),
        reasoning: row.reasoning.unwrap_or(0.0),
        tiers,
    })
}

/// Identifies the source and date of the checked-in price snapshot.
#[must_use]
pub const fn price_source() -> (&'static str, &'static str, &'static str) {
    (PRICE_SOURCE, PRICE_SOURCE_URL, PRICE_FETCHED_AT)
}
