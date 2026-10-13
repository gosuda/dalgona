//! The rule value model shared by rule files and `dal.rule` records.
//!
//! Plain values only: this module compiles no regular expression, reads no
//! file, and decides no grant. The two checked invariants are the rule name
//! grammar ([`Name`]) and the origin trust order ([`Origin::rank`]).

use std::borrow::Borrow;
use std::fmt;
use std::path::PathBuf;

pub use dal_core::ext::{InterruptMode, RepeatMode};

/// Longest rule name in bytes.
pub const NAME_MAX_BYTES: usize = 64;

/// Longest condition source in bytes; set building skips longer sources.
pub const CONDITION_MAX_BYTES: usize = 1024;

/// A checked rule name with the grammar `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
///
/// Ordering is byte order of the text, which is the name order of buckets.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Name(Box<str>);

impl Name {
    /// Checks `text` against the rule name grammar.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        name_is_valid(text).then(|| Self(text.into()))
    }

    /// Returns the checked name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&dal_core::ext::Name> for Name {
    /// Converts an extension identifier; its grammar `[a-z][a-z0-9_-]{0,63}`
    /// is a subset of the rule name grammar, so the conversion cannot fail.
    fn from(name: &dal_core::ext::Name) -> Self {
        Self(name.as_str().into())
    }
}

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reports whether `text` matches `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
#[must_use]
pub fn name_is_valid(text: &str) -> bool {
    let bytes = text.as_bytes();
    match bytes.split_first() {
        Some((first, rest)) => {
            bytes.len() <= NAME_MAX_BYTES
                && first.is_ascii_alphanumeric()
                && rest
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        }
        None => false,
    }
}

/// Where a rule comes from.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Origin {
    /// A rule file in the user rule directory.
    User(PathBuf),
    /// A plugin rule file, or a plugin-level problem without a file.
    Plugin {
        /// The plugin name.
        plugin: Box<str>,
        /// The rule file inside the plugin, if any.
        path: Option<PathBuf>,
    },
    /// A rule file in the project rule directory.
    Project(PathBuf),
    /// A `dal.rule` record registered by plugin code.
    Record {
        /// The registering plugin name.
        plugin: Box<str>,
    },
}

impl Origin {
    /// Returns the trust rank; a lower rank wins a name.
    ///
    /// The order is `user(0) < plugin(1) < project(2)`, and a record ranks as
    /// its plugin, so a project file never replaces a user or plugin rule.
    /// Code-over-file inside one plugin is the set builder's tie break.
    #[must_use]
    pub fn rank(&self) -> u8 {
        match self {
            Self::User(_) => 0,
            Self::Plugin { .. } | Self::Record { .. } => 1,
            Self::Project(_) => 2,
        }
    }

    /// Renders the origin as a problem source: the file path of a user or
    /// project rule, and `plugin:<name>` for a plugin or record origin.
    #[must_use]
    pub fn source_label(&self) -> String {
        match self {
            Self::User(path) | Self::Project(path) => path.display().to_string(),
            Self::Plugin { plugin, .. } | Self::Record { plugin } => format!("plugin:{plugin}"),
        }
    }
}

/// One condition as written, before compilation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionSource {
    /// Zero-based position in the rule's condition list.
    pub index: usize,
    /// The regular-expression source; see [`CONDITION_MAX_BYTES`].
    pub src: Box<str>,
}

/// The stream content a rule watches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeSpec {
    /// Whether assistant text is watched.
    pub text: bool,
    /// Whether reasoning text is watched.
    pub thinking: bool,
    /// Which tool arguments are watched.
    pub tools: ToolScope,
}

impl ScopeSpec {
    /// Reports whether the scope reaches text, thinking, or at least one tool.
    #[must_use]
    pub fn reaches_any(&self) -> bool {
        self.text
            || self.thinking
            || match &self.tools {
                ToolScope::All => true,
                ToolScope::Tools(pats) => pats.iter().any(|pattern| pattern.available),
            }
    }
}

/// The tool arguments a rule watches; `Tools` with no entry watches none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolScope {
    /// Every tool's arguments.
    All,
    /// Only the listed tools.
    Tools(Vec<ToolPat>),
}

/// One tool entry of a scope, with an optional compiled path glob.
#[derive(Clone, Debug)]
pub struct ToolPat {
    /// The tool name.
    pub tool: Box<str>,
    /// Whether this tool is available to the current matcher.
    ///
    /// If the product inventory is unavailable during parsing, this remains
    /// true until the set builder can classify the name.
    pub available: bool,
    /// The compiled path glob the tool's path argument must match, if any.
    ///
    /// The original pattern is available through [`globset::GlobMatcher::glob`].
    pub glob: Option<globset::GlobMatcher>,
}

impl PartialEq for ToolPat {
    fn eq(&self, other: &Self) -> bool {
        self.tool == other.tool
            && self.available == other.available
            && match (&self.glob, &other.glob) {
                (Some(left), Some(right)) => left.glob() == right.glob(),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for ToolPat {}

/// What a fire of the rule does.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleAction {
    /// Stop the stream and retry with the reminder.
    Interrupt,
    /// Append the reminder after the response.
    Remind,
    /// Record the match only.
    Report,
}

/// The set bucket a rule lands in.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Bucket {
    /// Watched on the stream.
    Stream,
    /// Always in the prompt.
    Always,
    /// Listed in the rulebook by description.
    Rulebook,
    /// Not usable.
    Dropped,
}

/// One rule from either origin; the body is kept byte for byte.
#[derive(Clone, Debug)]
pub struct Rule {
    /// The checked rule name.
    pub name: Name,
    /// Where the rule comes from.
    pub origin: Origin,
    /// The rulebook description.
    pub description: Option<String>,
    /// One to sixteen condition sources.
    pub conditions: Vec<ConditionSource>,
    /// The watched stream content.
    pub scope: ScopeSpec,
    /// Path globs that select the rule.
    pub globs: Option<globset::GlobSet>,
    /// Agent-name globs that select the rule.
    pub agents: Option<globset::GlobSet>,
    /// Whether the rule is always in the prompt.
    pub always_apply: bool,
    /// Whether a match is only reported.
    pub report: bool,
    /// Whether the rule is on; a disabled rule claims no name.
    pub enabled: bool,
    /// The interrupt behavior, if set.
    pub interrupt_mode: Option<InterruptMode>,
    /// The repeat behavior, if set.
    pub repeat_mode: Option<RepeatMode>,
    /// Turns between repeats, if set.
    pub repeat_gap: Option<u16>,
    /// The judge question of a judged record rule.
    pub judge: Option<Box<str>>,
    /// The reminder body.
    pub body: String,
}

/// How serious a problem is.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Severity {
    /// Something was skipped; the rules report exits 1.
    Skipped,
    /// Informational only.
    Note,
}

/// What a problem is about.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProblemKind {
    /// A whole rule.
    Rule,
    /// One condition of a rule.
    Condition,
    /// A path or agent glob.
    Glob,
    /// A scope token.
    Scope,
    /// A rule file.
    File,
    /// A rule directory.
    Directory,
    /// The set as a whole.
    Set,
    /// A note about the set as a whole.
    SetNote,
}

/// One entry of the problem table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Problem {
    /// The source the problem is about.
    pub origin: Origin,
    /// What the problem is about.
    pub kind: ProblemKind,
    /// Why it is a problem.
    pub reason: String,
    /// What dal did about it.
    pub consequence: String,
    /// How serious it is.
    pub severity: Severity,
}

/// Returns the longest prefix of `text` of at most `max` bytes that ends on a
/// UTF-8 character boundary.
#[must_use]
pub fn cut_utf8(text: &str, max: usize) -> &str {
    if max >= text.len() {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use proptest::prelude::*;

    use super::{Name, Origin, ScopeSpec, ToolScope, cut_utf8, name_is_valid};

    fn user() -> Origin {
        Origin::User(PathBuf::from("/home/u/.dal/rules/a.md"))
    }

    fn plugin() -> Origin {
        Origin::Plugin {
            plugin: "p".into(),
            path: Some(PathBuf::from("/plugins/p/rules/a.md")),
        }
    }

    fn record() -> Origin {
        Origin::Record { plugin: "p".into() }
    }

    fn project() -> Origin {
        Origin::Project(PathBuf::from("/work/.dal/rules/a.md"))
    }

    #[test]
    fn name_grammar_edges() {
        let longest = format!("a{}", "b".repeat(63));
        let too_long = format!("a{}", "b".repeat(64));
        for good in [
            "a",
            "Z",
            "0",
            "no-sleep",
            "A.b_c-9",
            "9..",
            longest.as_str(),
        ] {
            assert!(name_is_valid(good), "{good:?}");
            assert_eq!(Name::parse(good).unwrap().as_str(), good);
        }
        for bad in [
            "",
            ".a",
            "-a",
            "_a",
            "a b",
            "a/b",
            "a:b",
            "é",
            "aé",
            "a\n",
            too_long.as_str(),
        ] {
            assert!(!name_is_valid(bad), "{bad:?}");
            assert!(Name::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn name_orders_by_bytes() {
        let mut names: Vec<Name> = ["b", "B", "a.b", "a-b", "a", "0"]
            .into_iter()
            .map(|text| Name::parse(text).unwrap())
            .collect();
        names.sort();
        let texts: Vec<&str> = names.iter().map(Name::as_str).collect();
        assert_eq!(texts, ["0", "B", "a", "a-b", "a.b", "b"]);
    }

    #[test]
    fn core_name_converts_unchanged() {
        let core = dal_core::ext::Name::parse("no_sleep-2").unwrap();
        assert_eq!(Name::from(&core).as_str(), "no_sleep-2");
    }

    #[test]
    fn project_never_outranks_user_or_plugin() {
        assert!(user().rank() < plugin().rank());
        assert!(plugin().rank() < project().rank());
        assert_eq!(record().rank(), plugin().rank());
        let mut claims = [project(), record(), plugin(), user()];
        claims.sort_by_key(Origin::rank);
        assert_eq!(claims[0], user());
        assert_eq!(claims[3], project());
    }

    #[test]
    fn source_label_names_file_or_plugin() {
        assert_eq!(user().source_label(), "/home/u/.dal/rules/a.md");
        assert_eq!(project().source_label(), "/work/.dal/rules/a.md");
        assert_eq!(plugin().source_label(), "plugin:p");
        assert_eq!(record().source_label(), "plugin:p");
        let bare = Origin::Plugin {
            plugin: "q".into(),
            path: None,
        };
        assert_eq!(bare.source_label(), "plugin:q");
    }

    #[test]
    fn scope_reach() {
        let none = ScopeSpec {
            text: false,
            thinking: false,
            tools: ToolScope::Tools(Vec::new()),
        };
        assert!(!none.reaches_any());
        assert!(
            ScopeSpec {
                tools: ToolScope::All,
                ..none.clone()
            }
            .reaches_any()
        );
        assert!(
            ScopeSpec {
                thinking: true,
                ..none
            }
            .reaches_any()
        );
    }

    #[test]
    fn cut_keeps_whole_codepoints() {
        assert_eq!(cut_utf8("héllo", 0), "");
        assert_eq!(cut_utf8("héllo", 1), "h");
        assert_eq!(cut_utf8("héllo", 2), "h");
        assert_eq!(cut_utf8("héllo", 3), "hé");
        assert_eq!(cut_utf8("a🦀", 4), "a");
        assert_eq!(cut_utf8("a🦀", 5), "a🦀");
        assert_eq!(cut_utf8("abc", 99), "abc");
    }

    proptest! {
        #[test]
        fn cut_is_longest_boundary_prefix(text in any::<String>(), max in 0usize..64) {
            let cut = cut_utf8(&text, max);
            prop_assert!(text.starts_with(cut));
            prop_assert!(cut.len() <= max);
            if let Some(next) = text[cut.len()..].chars().next() {
                prop_assert!(cut.len() + next.len_utf8() > max);
            }
        }
    }
}
