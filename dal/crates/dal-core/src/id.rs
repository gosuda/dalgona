use std::{fmt, marker::PhantomData, num::NonZeroU64, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::{Uuid, Variant};

/// An identifier does not have its required wire representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IdError {
    /// The value has an invalid kind, length, or encoding.
    #[error("invalid identifier")]
    Invalid,
}

struct IdVisitor<T>(PhantomData<T>);

impl<T: FromStr<Err = IdError>> de::Visitor<'_> for IdVisitor<T> {
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a canonical identifier string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<T, E> {
        value.parse().map_err(E::custom)
    }
}

macro_rules! text_codec {
    ($name:ident) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                deserializer.deserialize_str(IdVisitor(PhantomData))
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    };
}

macro_rules! uuid_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub struct $name(#[cfg_attr(feature = "schema", schemars(with = "String"))] Uuid);

        impl $name {
            /// Creates a UUID version 7 identifier from the current time.
            #[must_use]
            pub fn new_v7() -> Self {
                Self(Uuid::now_v7())
            }

            /// Parses a lowercase, hyphenated UUID version 7 identifier.
            ///
            /// # Errors
            /// Returns `IdError::Invalid` for any other representation or UUID kind.
            pub fn parse(value: &str) -> Result<Self, IdError> {
                let uuid = Uuid::try_parse(value).map_err(|_| IdError::Invalid)?;
                let mut encoded = Uuid::encode_buffer();
                if uuid.get_version_num() != 7
                    || uuid.get_variant() != Variant::RFC4122
                    || uuid.as_hyphenated().encode_lower(&mut encoded) != value
                {
                    return Err(IdError::Invalid);
                }
                Ok(Self(uuid))
            }

            /// Borrows the UUID value.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        text_codec!($name);
    };
}

uuid_id!(SessionId, "The identity of a persisted session.");
uuid_id!(RequestId, "The identity of a pending request.");
uuid_id!(JobId, "The identity of an owned background job.");

macro_rules! counter_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        #[serde(transparent)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Creates an identifier from a nonzero counter.
            #[must_use]
            pub const fn new(value: NonZeroU64) -> Self {
                Self(value)
            }

            /// Returns the nonzero counter value.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

counter_id!(EntryId, "A nonzero entry counter scoped to one session.");
counter_id!(TurnId, "A nonzero turn counter scoped to one open session.");
counter_id!(Gen, "The generation of a session view.");
counter_id!(Seq, "A nonzero sequence number in one journal.");
counter_id!(
    GenerationId,
    "The identity of an immutable extension generation."
);

/// A provider's opaque tool-call identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct CallId(Box<str>);

impl CallId {
    /// Retains the provider's identity text without normalization.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Borrows the identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The opaque identity of an in-process or wire client.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ClientId(Box<str>);

impl ClientId {
    /// Retains the client identity text without normalization.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Borrows the identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The BLAKE3 digest of content stored outside the journal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BlobId(#[cfg_attr(feature = "schema", schemars(with = "String"))] [u8; 32]);

impl BlobId {
    /// Hashes content to obtain its blob identity.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Parses the 64-character lowercase hexadecimal digest.
    ///
    /// # Errors
    /// Returns `IdError::Invalid` for any other representation.
    pub fn parse(value: &str) -> Result<Self, IdError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(IdError::Invalid);
        }
        let hash = blake3::Hash::from_hex(value).map_err(|_| IdError::Invalid)?;
        Ok(Self(*hash.as_bytes()))
    }

    /// Borrows the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        blake3::Hash::from_bytes(self.0).fmt(formatter)
    }
}

text_codec!(BlobId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_decoding_rejects_noncanonical_and_wrong_kind() {
        for value in [
            "018F0F62-3B00-7000-8000-000000000001",
            "018f0f62-3b00-4000-8000-000000000001",
            "018f0f62-3b00-7000-0000-000000000001",
            "018f0f623b007000800000000000000001",
        ] {
            let json = format!("\"{value}\"");
            assert!(sonic_rs::from_str::<SessionId>(&json).is_err());
            assert!(sonic_rs::from_str::<RequestId>(&json).is_err());
            assert!(sonic_rs::from_str::<JobId>(&json).is_err());
        }
    }

    #[test]
    fn blob_decoding_requires_lowercase_hex_string() {
        for value in [
            "F".repeat(64),
            "g".repeat(64),
            "0".repeat(63),
            "0".repeat(65),
        ] {
            assert!(sonic_rs::from_str::<BlobId>(&format!("\"{value}\"")).is_err());
        }
        assert!(sonic_rs::from_str::<BlobId>("[0,0,0,0]").is_err());
    }

    #[test]
    fn counter_decoding_rejects_zero() {
        assert!(sonic_rs::from_str::<EntryId>("0").is_err());
        assert!(sonic_rs::from_str::<TurnId>("0").is_err());
        assert!(sonic_rs::from_str::<Gen>("0").is_err());
        assert!(sonic_rs::from_str::<Seq>("0").is_err());
        assert!(sonic_rs::from_str::<GenerationId>("0").is_err());
    }
}
