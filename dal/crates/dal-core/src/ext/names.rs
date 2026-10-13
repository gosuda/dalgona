use super::{Deserialize, Deserializer, FromStr, RegistrationError, Serialize, de, fmt};

/// A checked identifier. [`Name::parse`] admits the extension grammar
/// `[a-z][a-z0-9_-]{0,63}`; [`Name::parse_mapped_tool`] admits the wider
/// mapped-tool grammar and is the only constructor of names outside it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Name(pub(super) Box<str>);

pub(super) fn valid_name(value: &str) -> bool {
    value.len() <= 64 && {
        let mut bytes = value.bytes();
        matches!(bytes.next(), Some(b'a'..=b'z'))
            && bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
    }
}

/// The longest mapped tool name in bytes.
pub const MAPPED_TOOL_NAME_MAX: usize = 200;

/// A mapped tool name that does not match its grammar.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NameError {
    /// The name is empty.
    #[error("mapped tool name is empty")]
    Empty,
    /// The name exceeds [`MAPPED_TOOL_NAME_MAX`] bytes.
    #[error("mapped tool name has {len} bytes; the limit is 200")]
    TooLong {
        /// The byte length of the rejected name.
        len: usize,
    },
    /// The first byte is not a lowercase ASCII letter.
    #[error("mapped tool name must start with a lowercase ASCII letter")]
    FirstByte,
    /// A later byte is outside `[A-Za-z0-9._-]`.
    #[error("mapped tool name has a byte outside [A-Za-z0-9._-] at index {index}")]
    Byte {
        /// The byte index of the first rejected byte.
        index: usize,
    },
}

fn check_mapped_tool(value: &str) -> Result<(), NameError> {
    let bytes = value.as_bytes();
    let Some(first) = bytes.first() else {
        return Err(NameError::Empty);
    };
    if bytes.len() > MAPPED_TOOL_NAME_MAX {
        return Err(NameError::TooLong { len: bytes.len() });
    }
    if !first.is_ascii_lowercase() {
        return Err(NameError::FirstByte);
    }
    match bytes
        .iter()
        .position(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    {
        Some(index) => Err(NameError::Byte { index }),
        None => Ok(()),
    }
}

pub(super) fn deserialize_tool_name<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Name, D::Error> {
    let value = Box::<str>::deserialize(deserializer)?;
    Name::parse_mapped_tool(&value).map_err(de::Error::custom)
}

pub(super) fn deserialize_tool_names<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Name>, D::Error> {
    let values = Vec::<Box<str>>::deserialize(deserializer)?;
    values
        .iter()
        .map(|value| Name::parse_mapped_tool(value).map_err(de::Error::custom))
        .collect()
}

pub(super) fn deserialize_optional_tool_names<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Box<[Name]>>, D::Error> {
    let values = Option::<Vec<Box<str>>>::deserialize(deserializer)?;
    values
        .map(|values| {
            values
                .iter()
                .map(|value| Name::parse_mapped_tool(value).map_err(de::Error::custom))
                .collect()
        })
        .transpose()
}

impl Name {
    /// Parses one extension identifier.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidName`] if the value is empty, too
    /// long, non-ASCII, or outside the identifier grammar.
    pub fn parse(value: &str) -> Result<Self, RegistrationError> {
        if valid_name(value) {
            Ok(Self(value.into()))
        } else {
            Err(RegistrationError::InvalidName { name: value.into() })
        }
    }

    /// Parses one tool name, including the mapped form
    /// `<skill>.<server>.<tool>`: a lowercase ASCII letter, then bytes from
    /// `[A-Za-z0-9._-]`, at most [`MAPPED_TOOL_NAME_MAX`] bytes in all. A
    /// trailing dot is legal, and every name [`Name::parse`] accepts is legal.
    ///
    /// # Errors
    /// Returns [`NameError`] naming the first violated rule.
    pub fn parse_mapped_tool(value: &str) -> Result<Self, NameError> {
        check_mapped_tool(value)?;
        Ok(Self(value.into()))
    }

    /// Returns the fixed `test` identifier that test contexts are minted under.
    #[must_use]
    pub fn test() -> Self {
        Self("test".into())
    }

    /// Returns the validated identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Name {
    type Err = RegistrationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for Name {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Box::<str>::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// A host command identifier: a checked [`Name`], `skill:<slug>`, or a
/// plugin command `<plugin>:<cmd>` whose two parts are checked [`Name`]s.
///
/// The `skill` plugin prefix is reserved for skill commands.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct CommandName(pub(super) Box<str>);

impl CommandName {
    /// Parses one command identifier.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidCommandName`] when the value is
    /// not a valid [`Name`], not `skill:` followed by a lowercase slug, and
    /// not `<plugin>:<cmd>` with two valid names and a plugin other than
    /// `skill`.
    pub fn parse(value: &str) -> Result<Self, RegistrationError> {
        if valid_name(value) || valid_qualified(value) {
            return Ok(Self(value.into()));
        }
        Err(RegistrationError::InvalidCommandName { name: value.into() })
    }

    /// Returns the validated command identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(super) fn valid_skill_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_qualified(value: &str) -> bool {
    let Some((plugin, command)) = value.split_once(':') else {
        return false;
    };
    if plugin == "skill" {
        return valid_skill_slug(command);
    }
    valid_name(plugin) && valid_name(command)
}

impl fmt::Display for CommandName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CommandName {
    type Err = RegistrationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for CommandName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Box::<str>::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// A synthetic model identifier with the grammar
/// `[a-z0-9-]+/[a-z0-9._-]+`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ModelId(pub(super) Box<str>);

impl ModelId {
    /// Parses one synthetic model identifier.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidModelId`] for any other spelling.
    pub fn parse(value: &str) -> Result<Self, RegistrationError> {
        if crate::model::ModelRoute::is_valid_synthetic_id(value) {
            return Ok(Self(value.into()));
        }
        Err(RegistrationError::InvalidModelId { id: value.into() })
    }

    /// Returns the validated identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ModelId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Box::<str>::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// The origin class of an extension declaration.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Supplied by the application itself.
    Builtin,
    /// Shipped as part of a bundled distribution.
    Bundled,
    /// Supplied by the user.
    User,
}

/// Controls where an extension-provided item is visible.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Visible to the model in its ordinary tool surface.
    Model,
    /// Available only when explicitly requested.
    Deferred,
    /// Available only to evaluation flows.
    EvalOnly,
}

impl Visibility {
    /// Parses a visibility literal.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidVisibility`] for any other value.
    pub fn parse(value: &str) -> Result<Self, RegistrationError> {
        match value {
            "model" => Ok(Self::Model),
            "deferred" => Ok(Self::Deferred),
            "eval_only" => Ok(Self::EvalOnly),
            _ => Err(RegistrationError::InvalidVisibility {
                value: value.into(),
            }),
        }
    }
}
