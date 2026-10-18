//! Decoder for the AWS shared `config` and `credentials` files.
//!
//! [`ProfileSet::parse`] takes the text of both files and returns typed
//! profile, `sso-session`, and `services` sections. It is a pure function of
//! its two arguments: it reads no file, environment variable, process, or
//! network, and it resolves no credential. Reading the files, decoding their
//! bytes to UTF-8, choosing a profile, and following `source_profile` chains
//! belong to the caller.
//!
//! # Reference grammar
//!
//! The parser follows `aws-runtime` 1.10.0, `env_config/parse.rs` and
//! `env_config/normalize.rs`, checked against that crate's
//! `test-data/profile-parser-tests.json`:
//!
//! - Lines split like [`str::lines`]. A line of spaces and tabs is blank. A
//!   line whose first character is `#` or `;` is a comment. A line that starts
//!   with `[` is a section header, a line that starts with a space or tab is a
//!   continuation of the previous property, and every other line is a property
//!   (`parse.rs:75-81`, `parse.rs:122-137`).
//! - A header is cut at the first `#` or `;` with no whitespace rule, must
//!   then end with `]`, and keeps the raw text between the brackets; the name
//!   is trimmed and split into prefix and suffix later (`parse.rs:203-216`,
//!   `normalize.rs:33-53`).
//! - A property line splits at its first `=`. Key and value are trimmed of
//!   spaces and tabs, and the key is ASCII-lowercased. A `#` or `;` starts a
//!   comment only when whitespace precedes it; otherwise it is part of the
//!   value. Quotes, backslashes, and further `=` or `:` characters in a value
//!   are kept byte for byte (`parse.rs:249-298`).
//! - A continuation is appended to the value of the property above it, joined
//!   with `\n` and trimmed of spaces and tabs; its text keeps any comment
//!   (`parse.rs:173-201`). Under an empty parent value each continuation must
//!   be a `key = value` sub-property (`parse.rs:60-73`); the pairs are kept
//!   as sub-properties, in addition to the whole value.
//! - `config` sections are `[default]`, `[profile name]`,
//!   `[sso-session name]`, and `[services name]`. `credentials` sections are
//!   bare `[name]`. A section whose name, and a property whose key, is not
//!   made of `A-Za-z0-9_-/.%@:+` is ignored, as is any section the file kind
//!   does not allow (`normalize.rs:55-97`, `normalize.rs:186-210`).
//!   `[services name]` sub-properties keep their case; a repeated sub-property
//!   key replaces the earlier value (`normalize.rs:155-181`,
//!   `normalize.rs:212-228`).
//! - Profile names are case-sensitive; property keys are ASCII
//!   case-insensitive. Identical section headers share one section and a
//!   repeated key replaces the earlier value. In `config`,
//!   `[profile default]` replaces `[default]`. `credentials` is merged after
//!   `config`, so its keys win (`parse.rs:210-212`, `normalize.rs:102-183`,
//!   `file.rs:52-55`, `section.rs:260-269`).
//!
//! The reference folds headers and properties through `HashMap`, which orders
//! two headers that differ only in spelling (`[profile foo]` and
//! `[profile  foo]`) arbitrarily. This decoder applies them in the order their
//! first spelling appears and keeps property order within a section.
//!
//! # Errors
//!
//! Malformed input fails with [`ProfileError`]: the file, the one-based line,
//! and a category whose message mirrors the reference, never text from the
//! input. Every input the reference accepts decodes; no valid grammar is
//! refused.
//!
//! # Redaction
//!
//! The `Debug` output of every decoded type lists names and hides values.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

/// The characters the reference treats as whitespace (`parse.rs:28`).
const WHITESPACE: &[char] = &[' ', '\t'];

/// The characters that start a comment (`parse.rs:29`).
const COMMENT: &[char] = &['#', ';'];

/// The name of the default profile.
const DEFAULT: &str = "default";

/// The `config` section prefix of a named profile.
const PROFILE_PREFIX: &str = "profile";

/// The `config` section prefix of an IAM Identity Center session.
const SSO_SESSION_PREFIX: &str = "sso-session";

/// The `config` section prefix of a service endpoint table.
const SERVICES_PREFIX: &str = "services";

/// Which of the two AWS shared files a text belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfileFile {
    /// `~/.aws/config`: sections are `default`, `profile <name>`,
    /// `sso-session <name>`, and `services <name>`.
    Config,
    /// `~/.aws/credentials`: sections are bare profile names.
    Credentials,
}

impl fmt::Display for ProfileFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Config => "config",
            Self::Credentials => "credentials",
        })
    }
}

/// Why a file text was refused. The message of every variant mirrors the
/// reference parser; no variant carries text from the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ProfileErrorKind {
    /// A property or continuation line precedes the first section header
    /// (`parse.rs:145`, `parse.rs:175`).
    #[error("Expected a profile definition")]
    PropertyBeforeSection,
    /// An indented line has no property above it in its section
    /// (`parse.rs:194`).
    #[error("Expected a property definition, found continuation")]
    ContinuationWithoutProperty,
    /// A section header does not end with `]` (`parse.rs:207-209`); the
    /// leading-`[` half of the reference check is unreachable after line
    /// classification.
    #[error("Profile definition must end with ']'")]
    UnterminatedSectionHeader,
    /// A property line has no `=` (`parse.rs:242`).
    #[error("Expected an '=' sign defining a property")]
    MissingEquals,
    /// A property line has an empty name (`parse.rs:237`).
    #[error("Property did not have a name")]
    EmptyPropertyName,
    /// A sub-property line has no `=` (`parse.rs:65-73`, `parse.rs:242`).
    #[error("Expected an '=' sign defining a sub-property")]
    SubPropertyMissingEquals,
    /// A sub-property line has an empty name (`parse.rs:65-73`,
    /// `parse.rs:237`).
    #[error("Sub-property did not have a name")]
    SubPropertyEmptyName,
}

/// A refused file text: the file, the one-based line, and the category. It
/// never contains text from the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProfileError {
    /// The file whose text was refused.
    pub(crate) file: ProfileFile,
    /// The one-based line of the refusal.
    pub(crate) line: usize,
    /// The category of the refusal.
    pub(crate) kind: ProfileErrorKind,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AWS {} file, line {}: {}",
            self.file, self.line, self.kind
        )
    }
}

impl std::error::Error for ProfileError {}

/// One property: its value, plus the sub-properties below it when the value's
/// first line is empty.
#[derive(Clone, Default, Eq, PartialEq)]
struct Property {
    /// The value; a multi-line value keeps its `\n` joins.
    value: String,
    /// The `key = value` pairs of the lines below an empty first line, by
    /// exact key.
    nested: BTreeMap<String, String>,
}

/// A `[profile ...]`, `[default]`, or `[sso-session ...]` section.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct ProfileSection {
    /// The properties by ASCII-lowercased key.
    properties: BTreeMap<String, Property>,
}

impl ProfileSection {
    /// The value of `key`, matched ASCII case-insensitively. A multi-line
    /// value, including its `\n` joins, is returned whole.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        let key = ascii_lowercase(key);
        self.properties
            .get(&*key)
            .map(|property| property.value.as_str())
    }

    /// Applies the properties of one section; a repeated key replaces the
    /// whole earlier property. Keys that are not identifiers are ignored,
    /// as the reference drops them (`normalize.rs:186-199`).
    fn merge(&mut self, properties: Vec<(String, String)>) {
        for (key, raw) in properties {
            if !is_identifier(&key) {
                continue;
            }
            let (value, nested) = split_raw_value(raw);
            self.properties.insert(key, Property { value, nested });
        }
    }
}

impl fmt::Debug for ProfileSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileSection")
            .field("keys", &self.properties.keys().collect::<Vec<_>>())
            .field("values", &"<redacted>")
            .finish()
    }
}

/// A `[services <name>]` section: per service, its `key = value`
/// sub-properties, such as `endpoint_url`.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct ServicesSection {
    /// The sub-properties by ASCII-lowercased service name, then exact key.
    services: BTreeMap<String, BTreeMap<String, String>>,
}

impl ServicesSection {
    /// Applies the properties of one section, re-splitting each service's
    /// raw value into sub-properties (`normalize.rs:155-181`).
    fn merge(&mut self, properties: Vec<(String, String)>) {
        for (service, raw) in properties {
            if !is_identifier(&service) {
                continue;
            }
            self.services
                .entry(service)
                .or_default()
                .extend(parse_sub_properties(&raw));
        }
    }
}

impl fmt::Debug for ServicesSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServicesSection")
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .field("values", &"<redacted>")
            .finish()
    }
}

/// The merged contents of one `config` text and one `credentials` text.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct ProfileSet {
    /// The profiles by exact, case-sensitive name.
    profiles: BTreeMap<String, ProfileSection>,
    /// The `sso-session` sections by exact name.
    sso_sessions: BTreeMap<String, ProfileSection>,
    /// The `services` sections by exact name.
    services: BTreeMap<String, ServicesSection>,
}

impl ProfileSet {
    /// Decodes the text of the `config` file and of the `credentials` file.
    /// Pass an empty string for a file that does not exist.
    ///
    /// `config` is applied first and `credentials` second, so a key set in
    /// both comes from `credentials`.
    ///
    /// # Errors
    ///
    /// [`ProfileError`] for the first refused line of `config`, or else of
    /// `credentials`. No partial set is returned.
    pub(crate) fn parse(config: &str, credentials: &str) -> Result<Self, ProfileError> {
        let mut set = Self::default();
        set.merge_file(ProfileFile::Config, config)?;
        set.merge_file(ProfileFile::Credentials, credentials)?;
        Ok(set)
    }

    /// The profile named exactly `name`.
    pub(crate) fn profile(&self, name: &str) -> Option<&ProfileSection> {
        self.profiles.get(name)
    }

    /// The `[sso-session name]` section named exactly `name`.
    pub(crate) fn sso_session(&self, name: &str) -> Option<&ProfileSection> {
        self.sso_sessions.get(name)
    }

    /// Applies one file text on top of the sections already present.
    fn merge_file(&mut self, file: ProfileFile, text: &str) -> Result<(), ProfileError> {
        let sections = parse_raw(file, text)?;
        let resolved: Vec<Option<Resolved>> = sections
            .iter()
            .map(|section| resolve(file, &section.header))
            .collect();
        // `[profile default]` replaces every `[default]` of the same file
        // (`normalize.rs:127-137`). A `credentials` file never yields
        // `Resolved::BareDefault`, so the flag is inert there.
        let ignore_bare_default = resolved
            .iter()
            .any(|target| matches!(target, Some(Resolved::Profile(name)) if name == DEFAULT));
        for (section, target) in sections.into_iter().zip(resolved) {
            match target {
                Some(Resolved::BareDefault) if ignore_bare_default => {}
                Some(target) => self.apply(target, section.properties),
                None => {}
            }
        }
        Ok(())
    }

    /// Applies the properties of one section to the section `target` names.
    fn apply(&mut self, target: Resolved, properties: Vec<(String, String)>) {
        match target {
            Resolved::BareDefault => {
                self.profiles
                    .entry(DEFAULT.to_owned())
                    .or_default()
                    .merge(properties);
            }
            Resolved::Profile(name) => self.profiles.entry(name).or_default().merge(properties),
            Resolved::SsoSession(name) => {
                self.sso_sessions.entry(name).or_default().merge(properties);
            }
            Resolved::Services(name) => self.services.entry(name).or_default().merge(properties),
        }
    }
}

impl fmt::Debug for ProfileSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileSet")
            .field("profiles", &self.profiles.keys().collect::<Vec<_>>())
            .field(
                "sso_sessions",
                &self.sso_sessions.keys().collect::<Vec<_>>(),
            )
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// One `[header]` block as parsed: the raw text between the brackets, the
/// properties in file order, and a side index of property positions. A
/// property value is the reference's raw value: its first line, then each
/// continuation joined with `\n`.
struct RawSection {
    /// The text between the brackets, untrimmed.
    header: String,
    /// The `(key, raw value)` pairs in file order, keys ASCII-lowercased. A
    /// repeated key replaced the earlier value, so keys are unique.
    properties: Vec<(String, String)>,
    /// The position of each key in `properties`, so replacement and
    /// continuation append need no scan.
    keys: BTreeMap<String, usize>,
}

/// What a section header names, for one file kind.
enum Resolved {
    /// `[default]` in `config`.
    BareDefault,
    /// `[profile name]` in `config`, or `[name]` in `credentials`.
    Profile(String),
    /// `[sso-session name]` in `config`.
    SsoSession(String),
    /// `[services name]` in `config`.
    Services(String),
}

/// Why a property line was refused, before the file kind names the category
/// (`parse.rs:226-246`).
enum PropertyError {
    /// The line has no `=`.
    NoEquals,
    /// The line has an empty name.
    NoName,
}

impl PropertyError {
    /// The typed category for a property or a sub-property line.
    fn kind(self, sub_property: bool) -> ProfileErrorKind {
        match (self, sub_property) {
            (Self::NoEquals, false) => ProfileErrorKind::MissingEquals,
            (Self::NoEquals, true) => ProfileErrorKind::SubPropertyMissingEquals,
            (Self::NoName, false) => ProfileErrorKind::EmptyPropertyName,
            (Self::NoName, true) => ProfileErrorKind::SubPropertyEmptyName,
        }
    }
}

/// Parses a file text into raw sections in file order (`parse.rs:122-137`).
/// Side indices give header dedup and key replacement in `O(log n)`.
fn parse_raw(file: ProfileFile, text: &str) -> Result<Vec<RawSection>, ProfileError> {
    let refuse = |line: usize, kind: ProfileErrorKind| ProfileError { file, line, kind };
    let mut sections: Vec<RawSection> = Vec::new();
    let mut headers: BTreeMap<String, usize> = BTreeMap::new();
    let mut current: Option<usize> = None;
    let mut property: Option<usize> = None;
    let mut is_subproperty = false;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if is_empty_line(line) || is_comment_line(line) {
            continue;
        }
        if line.starts_with('[') {
            let header = read_profile_line(line).map_err(|kind| refuse(number, kind))?;
            current = Some(upsert_section(&mut sections, &mut headers, header));
            property = None;
            is_subproperty = false;
        } else if line.starts_with(WHITESPACE) {
            let Some(section) = current else {
                return Err(refuse(number, ProfileErrorKind::PropertyBeforeSection));
            };
            let Some(position) = property else {
                return Err(refuse(
                    number,
                    ProfileErrorKind::ContinuationWithoutProperty,
                ));
            };
            if is_subproperty {
                validate_subproperty(line).map_err(|kind| refuse(number, kind))?;
            }
            let raw = &mut sections[section].properties[position].1;
            raw.push('\n');
            raw.push_str(line.trim_matches(WHITESPACE));
        } else {
            let Some(section) = current else {
                return Err(refuse(number, ProfileErrorKind::PropertyBeforeSection));
            };
            let (key, value) =
                parse_property_line(line).map_err(|error| refuse(number, error.kind(false)))?;
            is_subproperty = value.is_empty();
            property = Some(insert_property(&mut sections[section], key, value));
        }
    }
    Ok(sections)
}

/// Returns the section position for `header`, creating it on first
/// appearance (`parse.rs:210-212`).
fn upsert_section(
    sections: &mut Vec<RawSection>,
    headers: &mut BTreeMap<String, usize>,
    header: String,
) -> usize {
    if let Some(&existing) = headers.get(header.as_str()) {
        existing
    } else {
        let position = sections.len();
        headers.insert(header.clone(), position);
        sections.push(RawSection {
            header,
            properties: Vec::new(),
            keys: BTreeMap::new(),
        });
        position
    }
}

/// Inserts or replaces a property and returns its position (`parse.rs:158`).
fn insert_property(section: &mut RawSection, key: Cow<'_, str>, value: &str) -> usize {
    if let Some(position) = section.keys.get(&*key).copied() {
        value.clone_into(&mut section.properties[position].1);
        position
    } else {
        let position = section.properties.len();
        let key = key.into_owned();
        section.keys.insert(key.clone(), position);
        section.properties.push((key, value.to_owned()));
        position
    }
}

/// Parses a header line into the raw bracket text (`parse.rs:203-216`).
fn read_profile_line(line: &str) -> Result<String, ProfileErrorKind> {
    let prepared = prepare_line(line, false);
    let Some(name) = prepared.strip_prefix('[') else {
        return Err(ProfileErrorKind::UnterminatedSectionHeader);
    };
    let Some(name) = name.strip_suffix(']') else {
        return Err(ProfileErrorKind::UnterminatedSectionHeader);
    };
    Ok(name.to_owned())
}

/// Validates an indented line below an empty parent value (`parse.rs:60-73`).
fn validate_subproperty(line: &str) -> Result<(), ProfileErrorKind> {
    if line.trim_matches(WHITESPACE).is_empty() {
        return Ok(());
    }
    parse_property_line(line)
        .map(|_| ())
        .map_err(|error| error.kind(true))
}

/// Parses a property line into a key-value pair (`parse.rs:249-262`).
fn parse_property_line(line: &str) -> Result<(Cow<'_, str>, &str), PropertyError> {
    let line = prepare_line(line, true);
    let Some((key, value)) = line.split_once('=') else {
        return Err(PropertyError::NoEquals);
    };
    let key = key.trim_matches(WHITESPACE);
    let value = value.trim_matches(WHITESPACE);
    if key.is_empty() {
        return Err(PropertyError::NoName);
    }
    Ok((ascii_lowercase(key), value))
}

/// Splits a raw value for the typed model, keeping it whole and projecting
/// sub-properties when the first line is empty.
fn split_raw_value(raw: String) -> (String, BTreeMap<String, String>) {
    let nested = if raw.starts_with('\n') {
        parse_sub_properties(&raw).into_iter().collect()
    } else {
        BTreeMap::new()
    };
    (raw, nested)
}

/// Splits a raw value into `key = value` pairs (`normalize.rs:212-228`).
fn parse_sub_properties(raw: &str) -> Vec<(String, String)> {
    raw.split('\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            line.split_once('=').map(|(key, value)| {
                (
                    key.trim_matches(WHITESPACE).to_owned(),
                    value.trim_matches(WHITESPACE).to_owned(),
                )
            })
        })
        .collect()
}

/// Names what a header addresses in the given file kind, or `None` when the
/// reference ignores the section (`normalize.rs:33-97`).
fn resolve(file: ProfileFile, header: &str) -> Option<Resolved> {
    let header = header.trim_matches(WHITESPACE);
    let (prefix, suffix) = match header.split_once(WHITESPACE) {
        Some((prefix, suffix)) => (Some(prefix.trim()), suffix.trim()),
        None => (None, header.trim()),
    };
    match (file, prefix) {
        (ProfileFile::Config, Some(prefix)) => resolve_prefixed(prefix, suffix),
        (ProfileFile::Config, None) => (suffix == DEFAULT).then_some(Resolved::BareDefault),
        (ProfileFile::Credentials, Some(_)) => None,
        (ProfileFile::Credentials, None) => {
            is_identifier(suffix).then(|| Resolved::Profile(suffix.to_owned()))
        }
    }
}

/// Names a `config` header of the form `[prefix suffix]`.
fn resolve_prefixed(prefix: &str, suffix: &str) -> Option<Resolved> {
    if !is_identifier(suffix) {
        return None;
    }
    let name = suffix.to_owned();
    match prefix {
        PROFILE_PREFIX => Some(Resolved::Profile(name)),
        SSO_SESSION_PREFIX => Some(Resolved::SsoSession(name)),
        SERVICES_PREFIX => Some(Resolved::Services(name)),
        _ => None,
    }
}

/// Whether `input` is made of `A-Za-z0-9_-/.%@:+`, the reference's identifier
/// alphabet (`normalize.rs:202-210`). The empty string passes, as there.
fn is_identifier(input: &str) -> bool {
    input.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '/' | '.' | '%' | '@' | ':' | '+')
    })
}

/// Lowercases ASCII letters, borrowing when there is nothing to change
/// (`parse.rs:264-270`).
fn ascii_lowercase(input: &str) -> Cow<'_, str> {
    if input.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(input.to_ascii_lowercase())
    } else {
        Cow::Borrowed(input)
    }
}

/// Whether a line is blank: spaces and tabs only (`parse.rs:75-77`).
fn is_empty_line(line: &str) -> bool {
    line.trim_matches(WHITESPACE).is_empty()
}

/// Whether a line starts with a comment character, untrimmed
/// (`parse.rs:79-81`): an indented `#` line is a continuation, not a comment.
fn is_comment_line(line: &str) -> bool {
    line.starts_with(COMMENT)
}

/// Trims a line and cuts it at the comment start (`parse.rs:282-298`).
fn prepare_line(line: &str, comments_need_whitespace: bool) -> &str {
    let line = line.trim_matches(WHITESPACE);
    let mut after_whitespace = !comments_need_whitespace;
    let mut comment_start = None;
    for (index, ch) in line.char_indices() {
        if after_whitespace && COMMENT.contains(&ch) {
            comment_start = Some(index);
            break;
        }
        after_whitespace = ch.is_whitespace() || !comments_need_whitespace;
    }
    comment_start
        .map_or(line, |index| &line[..index])
        .trim_matches(WHITESPACE)
}

#[cfg(test)]
mod tests;
