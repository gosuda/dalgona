//! Skill validation, the immutable skill registry, and the `skills` prompt
//! section.
//!
//! Every check runs at the input boundary in [`validate_registration`]: a
//! [`RegisteredSkill`] only exists for a well-formed name, a trimmed and
//! bounded description, a contained body path, and a complete UTF-8 body.
//! Nothing here touches the disk, parses frontmatter, or evaluates a plugin;
//! the caller hands in the bytes it read, and those bytes are the body.
//! [`SkillRegistry::merge`] then settles name claims once, in plugin
//! directory-name order, and the result never changes for the life of the
//! process.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::letter::LetterAssembly;

/// Largest accepted skill body, in bytes.
pub const MAX_BODY_BYTES: usize = 131_072;

/// Largest accepted trimmed description, in Unicode scalar values.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;

/// Largest accepted skill name, in bytes (all ASCII).
const MAX_NAME_BYTES: usize = 64;

/// The fixed first two lines of the `skills` prompt section.
const SECTION_HEAD: &str =
    "# Skills\nLoad a skill body before you follow it with the read tool at skill://NAME.";

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
    Ok(RegisteredSkill {
        plugin: plugin.into(),
        name: name.into(),
        description: description.into(),
        body: Arc::from(text),
        letter2image,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::letter::{FallbackReason, LetterChunk, LetterFallback};

    const HEAD: &str =
        "# Skills\nLoad a skill body before you follow it with the read tool at skill://NAME.";

    fn dir(plugin: &str) -> PathBuf {
        Path::new("/plugins").join(plugin)
    }

    fn register(
        plugin: &str,
        name: &str,
        description: &str,
        path: &str,
        body: BodyInput<'_>,
    ) -> Result<RegisteredSkill, SkillError> {
        validate_registration(
            plugin,
            &dir(plugin),
            SkillRegistration {
                name,
                description,
                path: Path::new(path),
                letter2image: false,
            },
            body,
        )
    }

    fn skill(plugin: &str, name: &str, description: &str, marked: bool) -> RegisteredSkill {
        let body = format!("body of {name}");
        let mut skill = register(
            plugin,
            name,
            description,
            "SKILL.md",
            BodyInput::Bytes(body.as_bytes()),
        )
        .unwrap();
        skill.letter2image = marked;
        skill
    }

    fn empty() -> LetterAssembly {
        LetterAssembly {
            chunks: Vec::new(),
            fallbacks: Vec::new(),
        }
    }

    fn chunk(id: &str, skill: &RegisteredSkill) -> LetterChunk {
        LetterChunk {
            id: id.into(),
            skill: skill.name.clone(),
            plugin: skill.plugin.clone(),
            source_text: Arc::from(&*skill.description),
            png: Arc::from(&b"\x89PNG"[..]),
            width: 8,
            height: 16,
            cell: [8, 16],
        }
    }

    fn fallback(skill: &RegisteredSkill, reason: FallbackReason) -> LetterFallback {
        LetterFallback {
            skill: skill.name.clone(),
            plugin: skill.plugin.clone(),
            source_text: Arc::from(&*skill.description),
            first_undrawable: None,
            reason,
        }
    }

    #[test]
    fn skill_name_grammar() {
        let long = "a".repeat(65);
        for name in [
            "Focus",
            "-bad",
            "a b",
            long.as_str(),
            "",
            "a_b",
            "caf\u{e9}",
        ] {
            let err = register("p", name, "d", "SKILL.md", BodyInput::Bytes(b"x")).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "skill name '{name}' is invalid: use 1 to 64 characters of a-z, 0-9, and '-', starting with a letter or digit"
                )
            );
        }
        let max = "9".repeat(64);
        for name in ["ok-name", "0", "a-", max.as_str(), "read", "patch"] {
            let ok = register("p", name, "d", "SKILL.md", BodyInput::Bytes(b"x")).unwrap();
            assert_eq!(&*ok.name, name);
        }
    }

    #[test]
    fn description_validation_errors() {
        let over = "\u{ac00}".repeat(MAX_DESCRIPTION_CHARS + 1);
        let cases = [
            ("", "skill description is empty"),
            (" \t\u{3000}\n", "skill description is empty"),
            (over.as_str(), "skill description exceeds 4096 characters"),
            (
                "ring\u{7}bell",
                "skill description contains control character U+0007",
            ),
            (
                "next\u{85}line",
                "skill description contains control character U+0085",
            ),
        ];
        for (description, expected) in cases {
            let err =
                register("p", "s", description, "SKILL.md", BodyInput::Bytes(b"x")).unwrap_err();
            assert_eq!(err.to_string(), expected);
        }

        let at_limit = format!("  {}\n", "\u{ac00}".repeat(MAX_DESCRIPTION_CHARS));
        let ok = register("p", "s", &at_limit, "SKILL.md", BodyInput::Bytes(b"x")).unwrap();
        assert_eq!(ok.description.chars().count(), MAX_DESCRIPTION_CHARS);
        assert_eq!(&*ok.description, at_limit.trim());
    }

    #[test]
    fn body_file_errors() {
        for path in [
            "../SKILL.md",
            "a/../../SKILL.md",
            "a/../SKILL.md",
            "/etc/passwd",
            "",
            ".",
        ] {
            let err = register("p", "s", "d", path, BodyInput::Bytes(b"x")).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("skill body path '{path}' must be relative to the plugin directory")
            );
        }
        // An escaping path is rejected before the body is consulted.
        let err = register("p", "s", "d", "../x", BodyInput::Missing).unwrap_err();
        assert!(matches!(err, SkillError::PathEscape { .. }));

        let at = dir("p").join("skills/SKILL.md");
        let at = at.display();
        let err = register("p", "s", "d", "skills/SKILL.md", BodyInput::Missing).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("skill body at '{at}' does not exist")
        );

        let err = register(
            "p",
            "s",
            "d",
            "skills/SKILL.md",
            BodyInput::Bytes(b"ok\xff\xfe"),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("skill body at '{at}' is not valid UTF-8")
        );

        // A multi-byte scalar cut at the end is invalid, not silently dropped.
        let err = register(
            "p",
            "s",
            "d",
            "skills/SKILL.md",
            BodyInput::Bytes(b"\xea\xb0"),
        )
        .unwrap_err();
        assert!(matches!(err, SkillError::BodyNotUtf8 { .. }));

        let over = vec![b'a'; MAX_BODY_BYTES + 1];
        let err = register("p", "s", "d", "skills/SKILL.md", BodyInput::Bytes(&over)).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("skill body at '{at}' exceeds 131072 bytes")
        );

        let limit = vec![b'a'; MAX_BODY_BYTES];
        let ok = register("p", "s", "d", "./skills/SKILL.md", BodyInput::Bytes(&limit)).unwrap();
        assert_eq!(ok.body.len(), MAX_BODY_BYTES);
    }

    #[test]
    fn body_served_byte_exact_after_source_edit() {
        let mut source =
            b"\xef\xbb\xbf---\r\nname: not-frontmatter\r\n---\r\n\t# Body \xea\xb0\x80  \n\n"
                .to_vec();
        let original = source.clone();
        let loaded = register("p", "s", "d", "SKILL.md", BodyInput::Bytes(&source)).unwrap();
        let registry = SkillRegistry::merge(&[("p", vec![loaded])]).unwrap();

        source.clear();
        source.extend_from_slice(b"edited on disk");

        assert_eq!(registry.body("s").unwrap().as_bytes(), original.as_slice());
        assert_eq!(registry.body("t"), None);
    }

    #[test]
    fn skill_name_collisions() {
        let first = skill("alpha", "focus", "alpha focus", false);
        let later = skill("zeta", "focus", "zeta focus", false);
        let later_other = skill("zeta", "other", "zeta other", false);
        let twice = [
            skill("mid", "focus", "one", false),
            skill("mid", "focus", "two", false),
        ];
        let mid_other = skill("mid", "solo", "mid solo", false);
        let clean = skill("omega", "clean", "omega clean", false);

        // Slice order is not merge order: `alpha` still merges first.
        let Err(conflict) = SkillRegistry::merge(&[
            ("zeta", vec![later_other, later]),
            ("omega", vec![clean]),
            ("mid", vec![mid_other, twice[0].clone(), twice[1].clone()]),
            ("alpha", vec![first]),
        ]) else {
            panic!("conflicting claims merged cleanly");
        };

        let messages: Vec<String> = conflict
            .rejections
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            messages,
            [
                "skill 'focus' is registered twice in plugin 'mid'",
                "skill 'focus' is already registered by plugin 'alpha'",
            ]
        );
        assert_eq!(
            conflict.to_string(),
            "skill 'focus' is registered twice in plugin 'mid'; skill 'focus' is already registered by plugin 'alpha'"
        );
        assert_eq!(conflict.rejections[1].plugin(), "zeta");

        let registry = conflict.registry;
        let names: Vec<&str> = registry.names().iter().map(|name| &**name).collect();
        assert_eq!(names, ["clean", "focus"]);
        let owners: Vec<(&str, &str)> = registry.iter().map(|s| (&*s.name, &*s.plugin)).collect();
        assert_eq!(owners, [("clean", "omega"), ("focus", "alpha")]);
        assert_eq!(&*registry.body("focus").unwrap(), "body of focus");
        // Rejected plugins leak none of their other records.
        assert_eq!(registry.body("other"), None);
        assert_eq!(registry.body("solo"), None);
    }

    #[test]
    fn shared_body_does_not_excuse_duplicate_name() {
        let body: Arc<str> = Arc::from("shared");
        let mut a = skill("p", "a", "d", false);
        let mut b = skill("p", "a", "e", false);
        a.body = Arc::clone(&body);
        b.body = body;
        let Err(conflict) = SkillRegistry::merge(&[("p", vec![a, b])]) else {
            panic!("duplicate name merged");
        };
        assert!(conflict.registry.names().is_empty());
        assert!(conflict.registry.section(&empty()).is_none());
    }

    #[test]
    fn foreign_record_rejects_submitting_plugin() {
        let stolen = skill("alpha", "focus", "d", false);
        let Err(conflict) = SkillRegistry::merge(&[("beta", vec![stolen])]) else {
            panic!("foreign record merged");
        };
        assert_eq!(
            conflict.to_string(),
            "skill 'focus' was validated for plugin 'alpha', not 'beta'"
        );
        assert!(conflict.registry.names().is_empty());
    }

    #[test]
    fn zero_skills_remove_section() {
        let registry = SkillRegistry::merge(&[("p", Vec::new())]).unwrap();
        assert_eq!(registry.section(&empty()), None);
        assert!(registry.names().is_empty());
    }

    #[test]
    fn section_forms_follow_byte_order_and_admission() {
        let image = skill("p", "b-image", "drawn text", true);
        let over = skill("p", "a-over", "over budget text", true);
        let plain = skill("q", "0-plain", "plain text", false);
        let upper = skill("q", "b", "sorts before b-image", false);
        let registry = SkillRegistry::merge(&[
            ("q", vec![upper, plain]),
            ("p", vec![image.clone(), over.clone()]),
        ])
        .unwrap();

        let names: Vec<&str> = registry.names().iter().map(|name| &**name).collect();
        assert_eq!(names, ["0-plain", "a-over", "b", "b-image"]);
        let iterated: Vec<&str> = registry.iter().map(|s| &*s.name).collect();
        assert_eq!(iterated, names);

        let plain_all = format!(
            "{HEAD}\n- 0-plain: plain text\n- a-over: over budget text\n- b: sorts before b-image\n- b-image: drawn text"
        );
        assert_eq!(
            registry.section(&empty()).as_deref(),
            Some(plain_all.as_str())
        );

        let assembly = LetterAssembly {
            chunks: vec![chunk("1", &image)],
            fallbacks: vec![fallback(&over, FallbackReason::OverBudget)],
        };
        let mixed = format!(
            "{HEAD}\n- 0-plain: plain text\n- a-over: over budget text\n- b: sorts before b-image\n- b-image"
        );
        let section = registry.section(&assembly).unwrap();
        assert_eq!(&*section, mixed);
        assert!(!section.ends_with('\n'));

        // A chunk naming the right skill under another plugin is not this
        // skill's image, so the description stays visible.
        let mut stranger = chunk("1", &image);
        stranger.plugin = "q".into();
        let assembly = LetterAssembly {
            chunks: vec![stranger],
            fallbacks: Vec::new(),
        };
        assert_eq!(
            registry.section(&assembly).as_deref(),
            Some(plain_all.as_str())
        );
    }

    #[test]
    fn section_call_stability() {
        let records = || {
            vec![
                (
                    "x",
                    vec![
                        skill("x", "zed", "last", true),
                        skill("x", "alpha", "first", false),
                    ],
                ),
                ("w", vec![skill("w", "mid", "middle", true)]),
            ]
        };
        let registry = SkillRegistry::merge(&records()).unwrap();
        let mid = registry.iter().find(|s| &*s.name == "mid").unwrap().clone();
        let assembly = LetterAssembly {
            chunks: vec![chunk("1", &mid)],
            fallbacks: Vec::new(),
        };
        let first = registry.section(&assembly).unwrap();
        let second = registry.section(&assembly).unwrap();
        let rebuilt = SkillRegistry::merge(&records()).unwrap();
        let third = rebuilt.section(&assembly).unwrap();
        assert_eq!(first, second);
        assert_eq!(first, third);
        assert_eq!(
            &*first,
            format!("{HEAD}\n- alpha: first\n- mid\n- zed: last")
        );
        let names: Vec<&str> = rebuilt.names().iter().map(|name| &**name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(names, sorted);
    }

    #[test]
    fn skill_capture_skip_rules() {
        let unmarked = skill("p", "plain", "unmarked text", false);
        let marked_empty = validate_registration(
            "p",
            &dir("p"),
            SkillRegistration {
                name: "marked",
                description: "   ",
                path: Path::new("SKILL.md"),
                letter2image: true,
            },
            BodyInput::Bytes(b"body"),
        );
        assert_eq!(marked_empty, Err(SkillError::EmptyDescription));

        let registry = SkillRegistry::merge(&[("p", vec![unmarked])]).unwrap();
        assert_eq!(registry.names().len(), 1);
        assert_eq!(
            registry.section(&empty()).as_deref(),
            Some(format!("{HEAD}\n- plain: unmarked text").as_str())
        );
    }
}
