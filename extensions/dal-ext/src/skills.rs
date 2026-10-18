//! Skill validation, the immutable skill registry, and the `skills` prompt
//! section.
//!
//! Every check runs at the input boundary in [`validate_registration`]: a
//! [`RegisteredSkill`] only exists for a well-formed name, a trimmed and
//! bounded description, a contained body path, and a complete UTF-8 body.
//! Nothing here touches the disk or evaluates a plugin; the caller hands in
//! the bytes it read, and those bytes are the body. The one front matter
//! read is the strict `mcp` object ([`dal_core::ext::decode_skill_mcp`]); the
//! body keeps every byte of the file, front matter included.
//! [`SkillRegistry::merge`] then settles name claims once, in plugin
//! directory-name order, and the result never changes for the life of its
//! generation: a reload publishes a new registry inside the new generation
//! snapshot instead of mutating state older turns still read.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use dal_agent::error::SchemeError;
use dal_agent::ext::{
    BoxFuture, Doc, Extension, ExtensionBuilder, PromptOrder, PromptSection, SchemeCx,
    SchemeResolver, SectionCx, SectionFn,
};
use dal_core::ext::{McpBlock, SkillFrontError, SkillRecord, decode_skill_mcp};
use dal_core::{BlobId, ModelInfo, RegistrationError, ServiceSet};

use crate::letter::{Font, LetterAssembly};

/// Largest accepted skill body, in bytes.
pub const MAX_BODY_BYTES: usize = 131_072;

/// Largest accepted trimmed description, in Unicode scalar values.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;

/// Largest accepted skill name, in bytes (all ASCII).
const MAX_NAME_BYTES: usize = 64;

/// The fixed first two lines of the `skills` prompt section.
const SECTION_HEAD: &str =
    "# Skills\nLoad a skill body before you follow it with the read tool at skill://NAME.";

/// Most skill names named in a missing-skill refusal.
///
/// [`SkillRegistry::names`] is already sorted bytewise, so the refusal lists
/// the first page in that order and counts the rest instead of pasting an
/// unbounded inventory into the error.
const MAX_AVAILABLE_SKILL_NAMES: usize = 20;

/// One skill as a plugin declared it, before validation.
#[derive(Clone, Copy, Debug)]
pub struct SkillRegistration<'a> {
    /// The requested skill name.
    pub name: &'a str,
    /// The untrimmed description.
    pub description: &'a str,
    /// The body path, relative to the plugin directory.
    pub path: &'a Path,
    /// Whether the skill opts into letter-to-image handling.
    pub letter2image: bool,
}

/// The body bytes the loader read for a registration.
#[derive(Clone, Copy, Debug)]
pub enum BodyInput<'a> {
    /// No file exists at the body path.
    Missing,
    /// The complete file content, byte for byte.
    Bytes(&'a [u8]),
}

/// A validated skill owned by one plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredSkill {
    /// The plugin that registered the skill.
    pub plugin: Box<str>,
    /// The validated skill name.
    pub name: Box<str>,
    /// The trimmed description.
    pub description: Box<str>,
    /// The body exactly as read.
    pub body: Arc<str>,
    /// Whether the skill opts into letter-to-image handling.
    pub letter2image: bool,
    /// The MCP servers the front matter declares, if any.
    pub mcp: Option<McpBlock>,
}

impl RegisteredSkill {
    /// Wraps one extension's validated [`SkillRecord`] with its owner tag.
    ///
    /// Rust extensions and converted Starlark extensions already validated
    /// their records at registration; [`SkillRegistry::merge`] only settles
    /// name claims across owners.
    #[must_use]
    pub fn from_record(plugin: &str, record: &SkillRecord) -> Self {
        Self {
            plugin: plugin.into(),
            name: record.name.as_str().into(),
            description: record.description.clone(),
            body: Arc::clone(&record.body),
            letter2image: record.letter2image,
            mcp: record.mcp.clone(),
        }
    }
}

/// A registration rejected by [`validate_registration`].
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SkillError {
    /// The name violates `^[a-z0-9][a-z0-9-]{0,63}$`.
    #[error(
        "skill name '{name}' is invalid: use 1 to 64 characters of a-z, 0-9, and '-', starting with a letter or digit"
    )]
    InvalidName {
        /// The rejected name as given.
        name: Box<str>,
    },
    /// The description is empty after trimming.
    #[error("skill description is empty")]
    EmptyDescription,
    /// The trimmed description exceeds [`MAX_DESCRIPTION_CHARS`].
    #[error("skill description exceeds 4096 characters")]
    DescriptionTooLong,
    /// The trimmed description contains a control character.
    #[error("skill description contains control character U+{codepoint:04X}")]
    DescriptionControl {
        /// The first control character found.
        codepoint: u32,
    },
    /// The body path is absolute, has a `..` component, or names no file
    /// below the plugin directory.
    #[error("skill body path '{}' must be relative to the plugin directory", path.display())]
    PathEscape {
        /// The rejected path as given.
        path: PathBuf,
    },
    /// No body file exists.
    #[error("skill body at '{}' does not exist", path.display())]
    BodyMissing {
        /// The body location below the plugin directory.
        path: PathBuf,
    },
    /// The body is not valid UTF-8.
    #[error("skill body at '{}' is not valid UTF-8", path.display())]
    BodyNotUtf8 {
        /// The body location below the plugin directory.
        path: PathBuf,
    },
    /// The body exceeds [`MAX_BODY_BYTES`].
    #[error("skill body at '{}' exceeds 131072 bytes", path.display())]
    BodyTooLarge {
        /// The body location below the plugin directory.
        path: PathBuf,
    },
    /// The front matter `mcp` object is malformed, at `path:line:col`.
    #[error(transparent)]
    Front(#[from] SkillFrontError),
}

/// Validates one registration and its body bytes into a [`RegisteredSkill`].
///
/// Checks run in order: name, description, path, body. The description is
/// kept trimmed; the body is kept byte-exact, including any byte-order mark
/// and line endings. Body errors name `plugin_dir` joined with the path.
///
/// # Errors
///
/// Returns the first [`SkillError`] the registration violates.
pub fn validate_registration(
    plugin: &str,
    plugin_dir: &Path,
    registration: SkillRegistration<'_>,
    body: BodyInput<'_>,
) -> Result<RegisteredSkill, SkillError> {
    let SkillRegistration {
        name,
        description,
        path,
        letter2image,
    } = registration;
    if !valid_name(name) {
        return Err(SkillError::InvalidName { name: name.into() });
    }
    let description = description.trim();
    if description.is_empty() {
        return Err(SkillError::EmptyDescription);
    }
    if description.chars().nth(MAX_DESCRIPTION_CHARS).is_some() {
        return Err(SkillError::DescriptionTooLong);
    }
    if let Some(control) = description.chars().find(|c| c.is_control()) {
        return Err(SkillError::DescriptionControl {
            codepoint: u32::from(control),
        });
    }
    if !contained(path) {
        return Err(SkillError::PathEscape { path: path.into() });
    }
    let bytes = match body {
        BodyInput::Missing => {
            return Err(SkillError::BodyMissing {
                path: plugin_dir.join(path),
            });
        }
        BodyInput::Bytes(bytes) => bytes,
    };
    if bytes.len() > MAX_BODY_BYTES {
        return Err(SkillError::BodyTooLarge {
            path: plugin_dir.join(path),
        });
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(SkillError::BodyNotUtf8 {
            path: plugin_dir.join(path),
        });
    };
    let mcp = decode_skill_mcp(&plugin_dir.join(path), text)?;
    Ok(RegisteredSkill {
        plugin: plugin.into(),
        name: name.into(),
        description: description.into(),
        body: Arc::from(text),
        letter2image,
        mcp,
    })
}

/// Whether `name` matches `^[a-z0-9][a-z0-9-]{0,63}$`.
fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let allowed = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    bytes.first().is_some_and(allowed)
        && bytes.len() <= MAX_NAME_BYTES
        && bytes.iter().all(|b| allowed(b) || *b == b'-')
}

/// Whether `path` lexically names an entry strictly below its base: only
/// normal and `.` components, at least one normal component.
fn contained(path: &Path) -> bool {
    let mut named = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => named = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    named
}

/// Why [`SkillRegistry::merge`] rejected one plugin.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PluginRejection {
    /// The plugin registers the same name more than once.
    #[error("skill '{name}' is registered twice in plugin '{plugin}'")]
    Twice {
        /// The rejected plugin.
        plugin: Box<str>,
        /// The repeated skill name.
        name: Box<str>,
    },
    /// An earlier plugin already owns the name.
    #[error("skill '{name}' is already registered by plugin '{other}'")]
    Claimed {
        /// The rejected plugin.
        plugin: Box<str>,
        /// The contested skill name.
        name: Box<str>,
        /// The earlier plugin that keeps the name.
        other: Box<str>,
    },
    /// A record was validated for a different plugin than the one that
    /// submits it.
    #[error("skill '{name}' was validated for plugin '{owner}', not '{plugin}'")]
    Foreign {
        /// The rejected plugin.
        plugin: Box<str>,
        /// The record's skill name.
        name: Box<str>,
        /// The plugin recorded in the skill.
        owner: Box<str>,
    },
}

impl PluginRejection {
    /// The plugin this rejection removed from the registry.
    #[must_use]
    pub fn plugin(&self) -> &str {
        match self {
            Self::Twice { plugin, .. }
            | Self::Claimed { plugin, .. }
            | Self::Foreign { plugin, .. } => plugin,
        }
    }
}

/// A merge that rejected at least one plugin.
///
/// The surviving plugins still form a complete registry: rejection removes
/// only the offending plugin's records, never an earlier owner's.
#[derive(Debug)]
pub struct SkillConflict {
    /// The registry of every plugin that was not rejected.
    pub registry: SkillRegistry,
    /// One rejection per rejected plugin, in merge order.
    pub rejections: Box<[PluginRejection]>,
}

impl fmt::Display for SkillConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, rejection) in self.rejections.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{rejection}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SkillConflict {}

/// The immutable set of registered skills, sorted bytewise by name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillRegistry {
    skills: Box<[RegisteredSkill]>,
    names: Box<[Box<str>]>,
}

impl SkillRegistry {
    /// An empty registry: no skills, so no section.
    ///
    /// Extension constructors start here; the host builds loaded registries
    /// through [`SkillRegistry::merge`].
    #[must_use]
    pub fn empty() -> Self {
        Self {
            skills: Box::new([]),
            names: Box::new([]),
        }
    }
    /// Merges the `skills` records of every extension into one registry.
    ///
    /// Each extension submits its records under its registered name, the
    /// same claim surface [`merge`](Self::merge) applies to plugin
    /// directories. Pass the complete extension batch: static, bundled, and
    /// plugin-derived alike.
    ///
    /// # Errors
    ///
    /// Returns [`SkillConflict`] when at least one extension's records were
    /// rejected; the surviving registry stays usable.
    pub fn merge_extensions(extensions: &[Extension]) -> Result<Self, SkillConflict> {
        let plugins: Vec<(&str, Vec<RegisteredSkill>)> = extensions
            .iter()
            .filter(|extension| !extension.skills().is_empty())
            .map(|extension| {
                (
                    extension.name(),
                    extension
                        .skills()
                        .iter()
                        .map(|record| RegisteredSkill::from_record(extension.name(), record))
                        .collect(),
                )
            })
            .collect();
        Self::merge(&plugins)
    }

    /// Merges validated skills from every plugin into one registry.
    ///
    /// Plugins merge in bytewise plugin-name order, the directory-name order
    /// of the scan, whatever the slice order. A plugin is accepted whole or
    /// rejected whole: a name repeated inside it, a name an earlier plugin
    /// already owns, or a record validated for another plugin rejects it,
    /// and it contributes no records. Later plugins still merge.
    ///
    /// # Errors
    ///
    /// Returns [`SkillConflict`], carrying the registry of the accepted
    /// plugins, when any plugin was rejected.
    pub fn merge(plugins: &[(&str, Vec<RegisteredSkill>)]) -> Result<Self, SkillConflict> {
        let mut order: Vec<&(&str, Vec<RegisteredSkill>)> = plugins.iter().collect();
        order.sort_by(|a, b| a.0.cmp(b.0));

        let mut accepted: Vec<&RegisteredSkill> = Vec::new();
        let mut rejections = Vec::new();
        for (plugin, skills) in order {
            match Self::claim(plugin, skills, &accepted) {
                Ok(()) => accepted.extend(skills),
                Err(rejection) => rejections.push(rejection),
            }
        }
        accepted.sort_by(|a, b| a.name.cmp(&b.name));

        let registry = Self {
            names: accepted.iter().map(|skill| skill.name.clone()).collect(),
            skills: accepted.into_iter().cloned().collect(),
        };
        if rejections.is_empty() {
            Ok(registry)
        } else {
            Err(SkillConflict {
                registry,
                rejections: rejections.into_boxed_slice(),
            })
        }
    }

    /// Checks one plugin's claims: its own records first (ownership, then
    /// repeated names), then names that accepted plugins already own.
    fn claim(
        plugin: &str,
        skills: &[RegisteredSkill],
        accepted: &[&RegisteredSkill],
    ) -> Result<(), PluginRejection> {
        let mut own = BTreeSet::new();
        for skill in skills {
            if *skill.plugin != *plugin {
                return Err(PluginRejection::Foreign {
                    plugin: plugin.into(),
                    name: skill.name.clone(),
                    owner: skill.plugin.clone(),
                });
            }
            if !own.insert(&*skill.name) {
                return Err(PluginRejection::Twice {
                    plugin: plugin.into(),
                    name: skill.name.clone(),
                });
            }
        }
        for skill in skills {
            if let Some(earlier) = accepted.iter().find(|earlier| earlier.name == skill.name) {
                return Err(PluginRejection::Claimed {
                    plugin: plugin.into(),
                    name: skill.name.clone(),
                    other: earlier.plugin.clone(),
                });
            }
        }
        Ok(())
    }

    /// Renders the `skills` prompt section, or `None` with no skills.
    ///
    /// One line per skill in bytewise name order, with no trailing newline.
    /// A marked skill with an admitted image in `assembly` shows only its
    /// name; every other skill, including a fallback, shows its description.
    #[must_use]
    pub fn section(&self, assembly: &LetterAssembly) -> Option<Box<str>> {
        if self.skills.is_empty() {
            return None;
        }
        let mut out = String::from(SECTION_HEAD);
        for skill in &self.skills {
            out.push_str("\n- ");
            out.push_str(&skill.name);
            let admitted = skill.letter2image
                && assembly
                    .chunks
                    .iter()
                    .any(|chunk| chunk.skill == skill.name && chunk.plugin == skill.plugin);
            if !admitted {
                out.push_str(": ");
                out.push_str(&skill.description);
            }
        }
        Some(out.into_boxed_str())
    }

    /// The body loaded at startup for `name`.
    #[must_use]
    pub fn body(&self, name: &str) -> Option<Arc<str>> {
        self.find(name).map(|skill| Arc::clone(&skill.body))
    }

    /// Every registered name, sorted bytewise.
    #[must_use]
    pub fn names(&self) -> &[Box<str>] {
        &self.names
    }

    /// Every registered skill, sorted bytewise by name.
    pub fn iter(&self) -> impl Iterator<Item = &RegisteredSkill> {
        self.skills.iter()
    }

    fn find(&self, name: &str) -> Option<&RegisteredSkill> {
        self.skills
            .binary_search_by(|skill| (*skill.name).cmp(name))
            .ok()
            .and_then(|index| self.skills.get(index))
    }
}

/// Renders the skills prompt section for the current letter assembly.
///
/// The registry and assembly share one immutable view so admitted images
/// appear by name and every other description remains text.
#[must_use]
pub fn section(registry: &SkillRegistry, assembly: &LetterAssembly) -> Option<Box<str>> {
    registry.section(assembly)
}

/// Reads skill bodies from its generation's immutable registry:
/// `skill://<name>`.
///
/// Bodies are served byte-exact from load time; the resolver never reads the
/// disk again. Each generation binds its own registry, so a reload cannot
/// leak new skill bodies into a turn still running on the old generation.
#[derive(Debug)]
pub struct SkillResolver {
    registry: Arc<SkillRegistry>,
}

impl SkillResolver {
    /// Builds a resolver over one generation's registry.
    #[must_use]
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
    }
}

impl SkillResolver {
    /// Resolves one `skill://` body or reports the unknown skill.
    ///
    /// A miss never falls back to a near name and never picks a skill: it
    /// names the requested path and the bounded `available:` list so the
    /// caller can retry with an exact name.
    fn lookup(&self, path: &str) -> Result<Doc, SchemeError> {
        if let Some(body) = self.registry.body(path) {
            return Ok(Doc::new(format!("skill://{path}"), body.to_string()));
        }
        let names = self.registry.names();
        let message = if names.is_empty() {
            format!("skill {path} does not exist; no skills are loaded")
        } else {
            format!(
                "skill {path} does not exist; available: {}",
                available_list(names)
            )
        };
        Err(SchemeError::Failed {
            message: message.into(),
        })
    }
}

/// The bounded `available:` list for a missing-skill refusal.
///
/// `names` arrives in the registry's bytewise order and keeps it; only the
/// tail is cut, with the hidden count after `…and N more`.
fn available_list(names: &[Box<str>]) -> String {
    if names.len() <= MAX_AVAILABLE_SKILL_NAMES {
        return names.join(", ");
    }
    let shown = names[..MAX_AVAILABLE_SKILL_NAMES].join(", ");
    format!(
        "{shown}, …and {} more",
        names.len() - MAX_AVAILABLE_SKILL_NAMES
    )
}

impl SchemeResolver for SkillResolver {
    fn read<'a>(
        &'a self,
        path: &'a str,
        _cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<Doc, SchemeError>> {
        let doc = self.lookup(path);
        Box::pin(async move { doc })
    }

    fn read_static(&self, path: &str) -> Option<Result<Doc, SchemeError>> {
        Some(self.lookup(path))
    }
}

/// One memoized draw plus its PNG digests in chunk order.
#[derive(Clone, Debug, Default)]
struct Memo {
    assembly: LetterAssembly,
    digests: Vec<BlobId>,
}

/// Dynamic `skills` prompt section over one generation's immutable registry.
///
/// Renders [`section`] against the letter assembly drawn from the captured
/// font; returns `None` when no skill is loaded. A skill's letter replaces
/// its description only for a model known to read images whose image the
/// request already carries, in history or current input: an
/// `image_input = false` model, a route that did not resolve, and any letter
/// the session never delivered all keep every description as text, so a
/// name-only line never hides a description the model never receives. A font
/// failure renders every description as text through an empty assembly, so
/// source delivery never depends on the bitmap table. The host composes the
/// first prompt through the same pure seams with its loaded registry.
#[derive(Debug, Clone)]
struct SkillsSectionFn {
    registry: Arc<SkillRegistry>,
    font: Arc<Font>,
    memo: Arc<OnceLock<Memo>>,
}

impl SkillsSectionFn {
    /// The drawn letters, computed once: the draw is a pure function of the
    /// immutable registry and font. Test-only handle; product paths go
    /// through [`SkillsSectionFn::assembly`].
    #[cfg(test)]
    pub(crate) fn drawn(&self) -> LetterAssembly {
        self.memoized().assembly.clone()
    }

    fn memoized(&self) -> &Memo {
        self.memo.get_or_init(|| {
            let assembly = crate::letter::letters(&self.registry, &self.font).unwrap_or_default();
            let digests = chunk_digests(&assembly);
            Memo { assembly, digests }
        })
    }

    /// The letter assembly this render shows: drawn letters whose images the
    /// request already carries. Delivery is read off the request being
    /// assembled, so a cancelled opening, a frozen mismatch, or a hook
    /// failure all fall back to text on the same request that lacks them.
    fn assembly(&self, model: Option<&ModelInfo>, delivered: &[BlobId]) -> LetterAssembly {
        if !model.is_some_and(|model| model.caps.image_input) {
            return LetterAssembly::default();
        }
        let memo = self.memoized();
        let chunks = memo
            .assembly
            .chunks
            .iter()
            .zip(&memo.digests)
            .filter(|(_, digest)| delivered.contains(digest))
            .map(|(chunk, _)| chunk.clone())
            .collect();
        LetterAssembly {
            chunks,
            fallbacks: memo.assembly.fallbacks.clone(),
        }
    }
}

/// PNG digests of one drawn assembly, in chunk order.
pub(crate) fn chunk_digests(assembly: &LetterAssembly) -> Vec<BlobId> {
    assembly
        .chunks
        .iter()
        .map(|chunk| BlobId::from_bytes(chunk.png.as_ref()))
        .collect()
}

impl SectionFn for SkillsSectionFn {
    fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        section(
            &self.registry,
            &self.assembly(cx.model, cx.delivered_images),
        )
        .map(str::into_string)
    }
}

/// Builds the `skills` extension: one dynamic skill prompt section, the
/// `skill` scheme, and the invocation surface.
///
/// Bodies are read through `skill://<name>`. They also enter a turn when its
/// prompt names the skill (`$name`, `$plugin:name`, or `/skill:name`), through
/// `/skill:<name>` commands, one per skill, and `/skill <name>`, which lists
/// candidates for any text that is not an exact name and runs nothing.
///
/// # Errors
///
/// Returns the runtime's typed build error when the builder rejects the
/// registration.
pub fn extension(registry: Arc<SkillRegistry>) -> Result<Extension, RegistrationError> {
    let resolver = SkillResolver::new(Arc::clone(&registry));
    let builder = ExtensionBuilder::new("skill", "0.1.0", ServiceSet::EMPTY)?
        .scheme("skill", Arc::new(resolver));
    let section = PromptSection::session(
        PromptOrder::Skills,
        Arc::new(SkillsSectionFn {
            registry: Arc::clone(&registry),
            font: Arc::new(Font::embedded()),
            memo: Arc::new(OnceLock::new()),
        }),
    );
    invoke::register(builder.prompt_section(section), registry)?.build()
}

mod invoke;

#[cfg(test)]
mod tests;
