//! Ordered edit-style input for patch dialect selection.

/// Raw ordered edit-style input; the patch consumer owns model glob rows.
///
/// A scalar selects one fixed style; a table holds ordered model glob rows.
/// The literal `default` key stays an entry and empty keys are kept;
/// validation (unknown style, missing default, empty key) lives in patch.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EditStyleInput {
    /// One fixed style or tier alias for every model.
    Scalar(Box<str>),
    /// Ordered model glob rows; `default` is the fallback entry.
    Table(Vec<(Box<str>, Box<str>)>),
}

impl Default for EditStyleInput {
    fn default() -> Self {
        Self::Scalar(Box::<str>::from("anchor"))
    }
}
