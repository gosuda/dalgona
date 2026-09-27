//! Owned raw JSON preserved byte for byte.
//!
//! Provider payloads and tool arguments are re-emitted verbatim: number
//! spellings, member order, and insignificant whitespace inside the raw
//! value survive a decode/encode cycle. Decoding never goes through a lossy
//! JSON DOM. The carrier and the tagged-decode helper below are thin users
//! of sonic-rs's public raw-value APIs; there is no second JSON engine and
//! no private protocol token in this module.

use std::{borrow::Cow, fmt};

use serde::de::{self, Deserializer};
use serde::ser::{Error as _, Serializer};
use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;

/// An owned, validated JSON text kept exactly as received.
///
/// Equality and hashing compare the raw text, not the parsed value: two
/// values that differ only in formatting are distinct. Build one with
/// [`RawJson::parse`] or decode one from Sonic input; both paths validate
/// that the text is exactly one JSON value.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct RawJson(Box<str>);

/// The text is not one valid JSON value, or a raw text no longer matches
/// the type it is decoded into.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid raw JSON: {message}")]
pub struct RawJsonError {
    /// Why the text was rejected.
    message: Box<str>,
}

impl RawJson {
    /// Validates the text as one JSON value and retains it verbatim.
    ///
    /// Surrounding JSON whitespace is dropped; spelling, member order,
    /// and interior whitespace are kept.
    ///
    /// # Errors
    /// Returns [`RawJsonError`] when the text is not exactly one JSON value.
    pub fn parse(value: &str) -> Result<Self, RawJsonError> {
        let trimmed = value.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
        sonic_rs::from_str::<sonic_rs::LazyValue<'_>>(trimmed)
            .map(|_| Self(trimmed.into()))
            .map_err(|error| RawJsonError {
                message: error.to_string().into(),
            })
    }

    /// Borrows the raw JSON text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Decodes the raw text into any JSON-backed value.
    ///
    /// # Errors
    /// Returns [`RawJsonError`] when the text no longer matches the target
    /// shape.
    pub fn decode_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, RawJsonError> {
        sonic_rs::from_str(self.as_str()).map_err(|error| RawJsonError {
            message: error.to_string().into(),
        })
    }
}

impl fmt::Debug for RawJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RawJson({})", self.0)
    }
}

impl Serialize for RawJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The stored text is one validated JSON value, so the root lookup
        // cannot fail.
        let value = sonic_rs::get_from_str(self.as_str(), sonic_rs::pointer![])
            .map_err(S::Error::custom)?;
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RawJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = sonic_rs::LazyValue::deserialize(deserializer)?;
        Ok(Self(value.as_raw_cow().into_owned().into_boxed_str()))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for RawJson {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RawJson".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Any JSON value is accepted; the wire surfaces part narrows this.
        schemars::Schema::from(true)
    }
}

/// A borrowed tagged JSON object held mid-decode: its raw text plus the one
/// discriminator member.
///
/// Tagged protocol enums decode through Sonic's public borrowed raw-value
/// carrier: the whole object stays raw, the discriminator is read by one
/// member scan, and each variant decodes its own fields from the raw text.
/// This bypasses serde's content buffering, which would re-encode raw
/// payloads through a lossy DOM. The discriminator may appear anywhere
/// among the members, and duplicates are rejected instead of silently
/// overwritten.
pub(crate) struct Tagged<'de> {
    raw: Cow<'de, str>,
    kind: Box<str>,
}

impl<'de> Tagged<'de> {
    /// Reads the one member named `tag` out of a raw JSON object.
    ///
    /// The whole object stays raw and the discriminator is read by one
    /// member scan, so each caller decodes variant fields from the raw
    /// text. This bypasses serde's content buffering, which would
    /// re-encode raw payloads through a lossy DOM and cannot decode a
    /// raw-value member at all. The discriminator may appear anywhere
    /// among the members, and duplicates are rejected instead of
    /// silently overwritten.
    fn scan<D: Deserializer<'de>>(
        deserializer: D,
        tag: &'static str,
    ) -> Result<(Cow<'de, str>, Box<str>), D::Error> {
        let value = sonic_rs::LazyValue::deserialize(deserializer)?;
        let raw = value.as_raw_cow();
        let members = value
            .into_object_iter()
            .ok_or_else(|| de::Error::custom(format!("expected a JSON object carrying `{tag}`")))?;
        let mut kind: Option<Box<str>> = None;
        for member in members {
            let (name, member) = member.map_err(de::Error::custom)?;
            if name != tag {
                continue;
            }
            if kind.is_some() {
                return Err(de::Error::custom(format!("duplicate field `{tag}`")));
            }
            let text = member
                .as_str()
                .ok_or_else(|| de::Error::custom(format!("field `{tag}` must be a string")))?;
            kind = Some(text.into());
        }
        let kind = kind.ok_or_else(|| de::Error::custom(format!("missing field `{tag}`")))?;
        Ok((raw, kind))
    }

    /// Reads the discriminator and checks it against `variants`.
    ///
    /// # Errors
    /// Fails when the value is not an object, or the discriminator is
    /// missing, duplicate, non-string, or not one of `variants`.
    pub(crate) fn decode<D: Deserializer<'de>>(
        deserializer: D,
        tag: &'static str,
        variants: &[&'static str],
    ) -> Result<Self, D::Error> {
        let (raw, kind) = Self::scan(deserializer, tag)?;
        if !variants.contains(&kind.as_ref()) {
            return Err(de::Error::custom(format!(
                "unknown `{tag}` variant `{kind}`"
            )));
        }
        Ok(Self { raw, kind })
    }

    /// Reads the discriminator without a variant allowlist.
    ///
    /// For surfaces with a catch-all variant: the caller matches known
    /// names and maps the rest. Still rejects a non-object, a missing
    /// or duplicate discriminator, and a non-string tag.
    ///
    /// # Errors
    /// Fails when the value is not an object or the discriminator is
    /// missing, duplicate, or non-string.
    pub(crate) fn decode_any<D: Deserializer<'de>>(
        deserializer: D,
        tag: &'static str,
    ) -> Result<Self, D::Error> {
        let (raw, kind) = Self::scan(deserializer, tag)?;
        Ok(Self { raw, kind })
    }

    /// The discriminator variant.
    #[must_use]
    pub(crate) fn kind(&self) -> &str {
        &self.kind
    }

    /// The whole raw text of the tagged value, for one variant-field decode.
    #[must_use]
    pub(crate) fn raw(&self) -> &str {
        &self.raw
    }
}

#[cfg(test)]
mod tests {
    #[derive(Debug, PartialEq)]
    enum Probe {
        Alpha { x: u64 },
        Beta,
    }

    #[derive(Deserialize)]
    struct AlphaWire {
        x: u64,
    }

    impl<'de> Deserialize<'de> for Probe {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let tagged = Tagged::decode(deserializer, "kind", &["alpha", "beta"])?;
            match tagged.kind() {
                "alpha" => {
                    let wire: AlphaWire =
                        sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                    Ok(Self::Alpha { x: wire.x })
                }
                "beta" => Ok(Self::Beta),
                other => Err(de::Error::custom(format!("unknown variant `{other}`"))),
            }
        }
    }
    use serde::de::DeserializeOwned;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn decode<T: DeserializeOwned>(json: &str) -> Result<T, RawJsonError> {
        RawJson::parse(json).and_then(|raw| raw.decode_as())
    }

    #[test]
    fn parse_keeps_bytes_and_rejects_garbage() -> TestResult {
        let raw = RawJson::parse(r#"{"b":2, "a":1e+02}"#)?;
        assert_eq!(raw.as_str(), r#"{"b":2, "a":1e+02}"#);
        assert!(RawJson::parse("nope").is_err());
        assert!(RawJson::parse("").is_err());
        assert!(RawJson::parse("1 2").is_err());
        assert!(RawJson::parse(r#"{"a":1} tail"#).is_err());
        Ok(())
    }

    #[test]
    fn json_whitespace_is_trimmed_and_other_boundaries_are_rejected() -> TestResult {
        let core = r#"{"a":1, "b":2}"#;
        for wrapped in [
            format!(" \t\r\n{core}\r\n\t "),
            format!("\t{core}"),
            format!("{core}\n"),
        ] {
            let parsed = RawJson::parse(&wrapped)?;
            assert_eq!(parsed.as_str(), core);
            let decoded: RawJson = sonic_rs::from_str(&wrapped)?;
            assert_eq!(decoded.as_str(), core);
        }

        for boundary in ["\u{b}", "\u{c}", "\u{a0}", "\u{3000}"] {
            for text in [format!("{boundary}1"), format!("1{boundary}")] {
                assert!(RawJson::parse(&text).is_err(), "parse accepted {text:?}");
                assert!(
                    sonic_rs::from_str::<RawJson>(&text).is_err(),
                    "deserialize accepted {text:?}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn boundary_characters_inside_json_strings_are_preserved() -> TestResult {
        let escaped_controls = "\\u000b\\u000c";
        let raw_spaces = "\u{a0}\u{3000}";
        let text = format!("\"{escaped_controls}{raw_spaces}\"");
        assert_eq!(RawJson::parse(&text)?.as_str(), text);
        assert_eq!(sonic_rs::from_str::<RawJson>(&text)?.as_str(), text);
        Ok(())
    }

    #[test]
    fn scalar_spellings_survive_a_round_trip() -> TestResult {
        for text in [
            "1e+02",
            "-0.0",
            "true",
            "false",
            "null",
            r#""a\nb""#,
            "-1e-9999",
        ] {
            let raw = RawJson::parse(text)?;
            assert_eq!(sonic_rs::to_string(&raw)?, text);
        }
        Ok(())
    }

    #[test]
    fn nested_raw_members_survive_an_envelope_round_trip() -> TestResult {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Envelope {
            v: u32,
            body: RawJson,
        }
        let envelope = Envelope {
            v: 1,
            body: RawJson::parse(r#"{"b":2, "a":1e+02}"#)?,
        };
        let bytes = sonic_rs::to_vec(&envelope)?;
        assert!(
            std::str::from_utf8(&bytes)?.contains(r#"{"b":2, "a":1e+02}"#),
            "raw body changed: {}",
            std::str::from_utf8(&bytes)?
        );
        let back: Envelope = sonic_rs::from_slice(&bytes)?;
        assert_eq!(back, envelope);
        Ok(())
    }

    #[test]
    fn decode_as_decodes_typed_values_and_rejects_shape_drift() -> TestResult {
        assert_eq!(decode::<u64>("41")?, 41);
        assert!(decode::<u64>(r#""41""#).is_err());
        assert!(RawJson::parse("{}")?.decode_as::<u64>().is_err());
        Ok(())
    }

    #[test]
    fn raw_texts_with_equal_values_but_different_bytes_are_distinct() -> TestResult {
        let compact = RawJson::parse(r#"{"a":1}"#)?;
        let spaced = RawJson::parse(r#"{ "a" : 1 }"#)?;
        assert_ne!(compact, spaced);
        assert_eq!(compact, RawJson::parse(r#"{"a":1}"#)?);
        Ok(())
    }

    #[test]
    fn tagged_decode_reads_the_discriminator_from_any_position() -> TestResult {
        assert_eq!(
            sonic_rs::from_str::<Probe>(r#"{"kind":"alpha","x":7}"#)?,
            Probe::Alpha { x: 7 }
        );
        assert_eq!(
            sonic_rs::from_str::<Probe>(r#"{"x":7,"kind":"alpha"}"#)?,
            Probe::Alpha { x: 7 }
        );
        assert_eq!(
            sonic_rs::from_str::<Probe>(r#"{"kind":"beta","x":7}"#)?,
            Probe::Beta
        );
        Ok(())
    }

    #[test]
    fn tagged_decode_rejects_bad_discriminators() {
        for text in [
            r#"{"kind":"alpha"}"#,                      // missing variant fields
            r#"{"x":7}"#,                               // missing discriminator
            r#"{"kind":"alpha","kind":"alpha","x":7}"#, // duplicate discriminator
            r#"{"kind":1,"x":7}"#,                      // non-string discriminator
            r#"{"kind":"gamma","x":7}"#,                // unknown discriminator
            r#""alpha""#,                               // not an object
            "[1,2]",                                    // not an object
        ] {
            assert!(
                sonic_rs::from_str::<Probe>(text).is_err(),
                "expected rejection of {text}"
            );
        }
    }
}
