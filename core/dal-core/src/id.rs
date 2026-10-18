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

/// The argument `dalgon -r` takes for a session, safe to paste into a shell.
///
/// Three tiers, checked in order:
///
/// - A name of only ASCII letters, digits, `.`, `_`, and `-` prints bare.
/// - A name of those characters plus spaces and apostrophes, and no other
///   character, prints in double quotes. POSIX shells, fish, `PowerShell`, and
///   `cmd.exe` read that form as typed: their double-quote expansions are `$`,
///   backticks, `\`, `"`, `!`, and `%`, and this tier holds none of them.
/// - Anything else prints `id`, as does an empty name or one that starts with
///   `-`. A `$` beside a space therefore takes this tier. The canonical
///   hyphenated `UUIDv7` form holds only hex digits and hyphens, and `-r`
///   resolves it.
#[must_use]
pub fn resume_token(name: &str, id: SessionId) -> String {
    let bare = |scalar: char| scalar.is_ascii_alphanumeric() || matches!(scalar, '.' | '_' | '-');
    if name.is_empty() || name.starts_with('-') {
        id.to_string()
    } else if name.chars().all(bare) {
        name.to_owned()
    } else if name
        .chars()
        .all(|scalar| bare(scalar) || matches!(scalar, ' ' | '\''))
    {
        format!("\"{name}\"")
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_token_prints_a_name_only_when_every_listed_shell_reads_it_as_typed() {
        const ID: &str = "018f0f62-3b00-7000-8000-000000000001";
        let id = SessionId::parse(ID).expect("fixed v7 id parses");
        for (name, token) in [
            ("demo", "demo"),
            ("my-session_1.x", "my-session_1.x"),
            ("my session", "\"my session\""),
            ("o'brien", "\"o'brien\""),
            ("demo$(calc)", ID),
            ("a`b", ID),
            ("foo\"; calc #", ID),
            ("a $b", ID),
            ("100%PATH%", ID),
            ("hi!", ID),
            ("a\\", ID),
            ("-rf", ID),
            ("a|b&c<d>e", ID),
            ("a\u{201c}; calc #", ID),
            ("\u{1F600}", ID),
            ("", ID),
        ] {
            assert_eq!(resume_token(name, id), token, "name {name:?}");
        }
    }

    #[test]
    fn uuid_decoding_accepts_canonical_v7_and_rejects_noncanonical() {
        let canonical = "01927f40-0000-7000-8000-000000000001";
        let session = SessionId::parse(canonical).expect("canonical v7 parses");
        assert_eq!(session.to_string(), canonical);
        assert_eq!(
            sonic_rs::from_str::<SessionId>(&format!("\"{canonical}\"")).expect("serde accepts"),
            session
        );
        let request = RequestId::parse(canonical).expect("canonical v7 parses");
        assert_eq!(request.to_string(), canonical);
        assert_eq!(
            sonic_rs::from_str::<RequestId>(&format!("\"{canonical}\"")).expect("serde accepts"),
            request
        );
        let job = JobId::parse(canonical).expect("canonical v7 parses");
        assert_eq!(job.to_string(), canonical);
        assert_eq!(
            sonic_rs::from_str::<JobId>(&format!("\"{canonical}\"")).expect("serde accepts"),
            job
        );
        for value in [
            "01927F40-0000-7000-8000-000000000001",
            "550e8400-e29b-41d4-a716-446655440000",
            "01927f40-0000-7000-0000-000000000001",
            "01927f40000070008000000000000001",
        ] {
            assert_eq!(SessionId::parse(value), Err(IdError::Invalid), "{value}");
            assert_eq!(RequestId::parse(value), Err(IdError::Invalid), "{value}");
            assert_eq!(JobId::parse(value), Err(IdError::Invalid), "{value}");
            let json = format!("\"{value}\"");
            assert!(sonic_rs::from_str::<SessionId>(&json).is_err(), "{value}");
            assert!(sonic_rs::from_str::<RequestId>(&json).is_err(), "{value}");
            assert!(sonic_rs::from_str::<JobId>(&json).is_err(), "{value}");
        }
    }

    #[test]
    fn blob_decoding_accepts_canonical_digest_and_rejects_variants() {
        let canonical = BlobId::from_bytes(b"stored").to_string();
        assert_eq!(canonical.len(), 64, "digest is 64 lowercase hex chars");
        let blob = BlobId::parse(&canonical).expect("canonical digest parses");
        assert_eq!(blob.to_string(), canonical);
        assert_eq!(
            sonic_rs::from_str::<BlobId>(&format!("\"{canonical}\"")).expect("serde accepts"),
            blob
        );
        for value in [
            canonical.to_uppercase(),
            "g".repeat(64),
            "0".repeat(63),
            "0".repeat(65),
        ] {
            assert_eq!(BlobId::parse(&value), Err(IdError::Invalid), "{value}");
            assert!(
                sonic_rs::from_str::<BlobId>(&format!("\"{value}\"")).is_err(),
                "{value}"
            );
        }
        assert!(sonic_rs::from_str::<BlobId>("[0,0,0,0]").is_err());
    }

    #[test]
    fn counter_decoding_accepts_one_and_rejects_zero_shapes() {
        assert_eq!(
            sonic_rs::from_str::<EntryId>("1").expect("1 parses").get(),
            1
        );
        assert_eq!(
            sonic_rs::from_str::<TurnId>("1").expect("1 parses").get(),
            1
        );
        assert_eq!(sonic_rs::from_str::<Gen>("1").expect("1 parses").get(), 1);
        assert_eq!(sonic_rs::from_str::<Seq>("1").expect("1 parses").get(), 1);
        assert_eq!(
            sonic_rs::from_str::<GenerationId>("1")
                .expect("1 parses")
                .get(),
            1
        );
        for value in ["0", "-1", "01"] {
            assert!(sonic_rs::from_str::<EntryId>(value).is_err(), "{value}");
            assert!(sonic_rs::from_str::<TurnId>(value).is_err(), "{value}");
            assert!(sonic_rs::from_str::<Gen>(value).is_err(), "{value}");
            assert!(sonic_rs::from_str::<Seq>(value).is_err(), "{value}");
            assert!(
                sonic_rs::from_str::<GenerationId>(value).is_err(),
                "{value}"
            );
        }
    }
}
