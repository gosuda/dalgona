//! Dialect registry: descriptions, grammars, tiers, and parse dispatch.

use super::ir::{DialectId, Edit, ParseError, Tier};

pub(crate) mod anchor;
pub(crate) mod apply_patch;
pub(crate) mod hashline;
pub(crate) mod hashline_enhanced;
pub(crate) mod hashline_light;
pub(crate) mod replace;

/// Parses one payload with the selected dialect.
pub(crate) fn parse(style: DialectId, input: &str, symbols: bool) -> Result<Vec<Edit>, ParseError> {
    match style {
        DialectId::Anchor => anchor::parse(input, symbols),
        DialectId::Replace => replace::parse(input, symbols),
        DialectId::Hashline => hashline::parse(input, symbols),
        DialectId::HashlineLight => hashline_light::parse(input, symbols),
        DialectId::HashlineEnhanced => hashline_enhanced::parse(input, symbols),
        DialectId::ApplyPatch => apply_patch::parse(input, symbols),
    }
}

/// Returns the prompt-intensity tier; the engine never branches on it.
#[must_use]
pub const fn tier(style: DialectId) -> Tier {
    match style {
        DialectId::Replace | DialectId::ApplyPatch => Tier::Simple,
        DialectId::Anchor => Tier::Balanced,
        DialectId::Hashline | DialectId::HashlineLight | DialectId::HashlineEnhanced => {
            Tier::Strict
        }
    }
}

/// Returns the wire name used in parse-error suffixes and history.
#[must_use]
pub const fn name(style: DialectId) -> &'static str {
    match style {
        DialectId::Anchor => "anchor",
        DialectId::Replace => "replace",
        DialectId::Hashline => "hashline",
        DialectId::HashlineLight => "hashline-light",
        DialectId::HashlineEnhanced => "hashline-enhanced",
        DialectId::ApplyPatch => "apply_patch",
    }
}

/// Appends the current-dialect reminder to every parse-class error.
#[must_use]
pub fn with_suffix(style: DialectId, error: ParseError) -> String {
    let ParseError { mut message, .. } = error;
    message.push_str("\nCurrent edit_style is \"");
    message.push_str(name(style));
    message.push_str(
        "\" for this model; past calls in history may use another dialect. Do not imitate them.",
    );
    message
}

/// Model-facing description bytes for one dialect and symbol flag.
#[must_use]
pub fn description(style: DialectId, symbols: bool) -> &'static str {
    match style {
        DialectId::Replace if !symbols => REPLACE_DESCRIPTION,
        DialectId::Replace => REPLACE_DESCRIPTION_SYMBOLS,
        DialectId::Anchor => ANCHOR_DESCRIPTION,
        DialectId::Hashline if symbols => HASHLINE_DESCRIPTION_SYMBOLS,
        DialectId::Hashline => HASHLINE_DESCRIPTION,
        DialectId::HashlineLight if symbols => HASHLINE_LIGHT_DESCRIPTION_SYMBOLS,
        DialectId::HashlineLight => HASHLINE_LIGHT_DESCRIPTION,
        DialectId::HashlineEnhanced if symbols => HASHLINE_ENHANCED_DESCRIPTION_SYMBOLS,
        DialectId::HashlineEnhanced => HASHLINE_ENHANCED_DESCRIPTION,
        DialectId::ApplyPatch => APPLY_PATCH_DESCRIPTION,
    }
}

const REPLACE_DESCRIPTION: &str = "Change files. Each entry of changes has a path and one action. old and new: replace old with new; copy old from read or search output without the <n>: prefixes. old must occur once; when it occurs more than once, add line, the line where your copy starts, or set all to true with the file's tag to replace every occurrence. Trailing spaces and curly quotes or dashes may differ; indentation must match. tag and new: replace the whole file; the tag is on the last line of a read that showed the whole file. create: make a new file. delete: true removes the file. rename: move the file. All changes to one file match its content from before this call and must not overlap. Line endings and a byte order mark are kept. The result shows each changed region as <n>:<text> lines.";

const REPLACE_DESCRIPTION_SYMBOLS: &str = "Change files. Each entry of changes has a path and one action. old and new: replace old with new; copy old from read or search output without the <n>: prefixes. old must occur once; when it occurs more than once, add line, the line where your copy starts, or set all to true with the file's tag to replace every occurrence. Trailing spaces and curly quotes or dashes may differ; indentation must match. tag and new: replace the whole file; the tag is on the last line of a read that showed the whole file. create: make a new file. delete: true removes the file. rename: move the file. All changes to one file match its content from before this call and must not overlap. Line endings and a byte order mark are kept. The result shows each changed region as <n>:<text> lines. symbol, tag, and new: replace a definition; search mode symbol shows it with its tag; new \"\" deletes it. at \"before\" or \"after\" inserts new next to it, without tag. symbol, old, and new: replace old inside that definition; old must occur there once; no tag needed.";

const ANCHOR_DESCRIPTION: &str = "Change files with one payload. *** File: path opens a file; a bare *** File: continues it; repeat for more files; every section applies atomically. Each edit is *** Find and one action. A Find body copies current text exactly, without read prefixes. *** Find @40 adds the line where the text starts; use it only when the text occurs more than once. *** Find 40-88 addresses a span: quote the file's current line 40 first and line 88 last, or quote every line of the span; you must have been shown every line of the span. *** Find all changes every match and needs the file tag: *** File: path #TAG, copied from a read that showed the whole file. Actions: *** Replace with the final text, empty deletes; *** Insert Before and *** Insert After add lines and keep the match. *** Replace File rewrites the whole file and needs the tag. *** New File: path plus a body creates; *** Delete File: path removes; *** Move: old -> new renames. Every edit addresses the file as it was before this payload, and edits to one file must not overlap. Trailing spaces and curly quotes or dashes may differ; indentation must match. The result shows each changed region with line numbers, so your next edit can copy from it. A failed payload changes nothing: fix it and resend the whole payload.";

const HASHLINE_DESCRIPTION_SYMBOLS: &str = "Hashline patches files. Each file: `[PATH#TAG]`, `TAG` required 4-hex snapshot from latest `read`/`search`. New file: `[PATH#NEW]` then `PUT >$:` with `+` rows. Numbers: original `LINE:TEXT`, never hunk-shifted.\n\n<ops>\n`PUT N.=M:` replace inclusive N–M with `+` body (`N.=N` for one line); `PUT N*:` replace block N.\n`PUT <N:`/`PUT >N:` insert before/after N (`<1` head, `>$` tail). `PUT >N*:` insert after block N at sibling depth; inside, use `PUT >M:` at closer.\n`CUT N.=M`/`CUT N*` delete.\n`REM` delete file; `MV DEST` rename after prior edits.\n</ops>\n\n<rules>\n- `:` ops only: body rows `+TEXT` verbatim incl. indent; lone `+` blank. Literal `- item`/`+ item` → `+- item`/`++ item`. NEVER `-`/bare context. Body length independent of range; delete with CUT, not empty PUT.\n- Touch displayed changed lines only; `…`, `..`, collapsed `N-M:` and out-of-window lines UNSEEN. Re-read first. Tight ranges: split nonadjacent changes; NEVER include keepers or start/end mid-expression/block. Pure addition uses gap PUT.\n- `*` requires multi-line opener, NEVER closer/last/inner statement; use range/gap for one statement. Anchor first decorator/attribute/doc-comment to include it; standalone comments need explicit range.\n- Markdown heading blocks run through deeper headings until next same/higher; after section `PUT >N*:`, end body with blank line.\n- NEVER restyle unrelated code. After EVERY edit tag/numbers change: use edit response or fresh `read`; stale tag/surprise → STOP, re-read.\n</rules>\n\n<example>\n[greet.py#A1B2]\nPUT 1*:\n+@cache\n+def greet(name):\n+    print(name)\n[PLAN.md#3C4D]\nPUT >2:\n+- task\n</example>";

const HASHLINE_DESCRIPTION: &str = "Hashline patches files. Each file: `[PATH#TAG]`, `TAG` required 4-hex snapshot from latest `read`/`search`. New file: `[PATH#NEW]` then `PUT >$:` with `+` rows. Numbers: original `LINE:TEXT`, never hunk-shifted.\n\n<ops>\n`PUT N.=M:` replace inclusive N–M with `+` body (`N.=N` for one line).\n`PUT <N:`/`PUT >N:` insert before/after N (`<1` head, `>$` tail).\n`CUT N.=M` delete.\n`REM` delete file; `MV DEST` rename after prior edits.\n</ops>\n\n<rules>\n- `:` ops only: body rows `+TEXT` verbatim incl. indent; lone `+` blank. Literal `- item`/`+ item` → `+- item`/`++ item`. NEVER `-`/bare context. Body length independent of range; delete with CUT, not empty PUT.\n- Touch displayed changed lines only; `…`, `..`, collapsed `N-M:` and out-of-window lines UNSEEN. Re-read first. Tight ranges: split nonadjacent changes; NEVER include keepers or start/end mid-expression/block. Pure addition uses gap PUT.\n- Markdown heading blocks run through deeper headings until next same/higher.\n- NEVER restyle unrelated code. After EVERY edit tag/numbers change: use edit response or fresh `read`; stale tag/surprise → STOP, re-read.\n</rules>\n\n<example>\n[greet.py#A1B2]\nPUT 1.=2:\n+@cache\n+def greet(name):\n+    print(name)\n[PLAN.md#3C4D]\nPUT >2:\n+- task\n</example>";

const HASHLINE_LIGHT_DESCRIPTION_SYMBOLS: &str = "Edit files from a read/search snapshot. Copy its [path@reference] header.\nPUT N: or PUT N-M: replaces original lines using + body rows. CUT N or\nCUT N-M removes them. PUT <N:, PUT >N:, and PUT >$: insert in original\nsnapshot gaps. N* operations require enabled symbol support.\n[PATH#NEW] plus PUT >$: creates a file. REM explicitly deletes; MV DEST\nrenames after edits. REM/MV require the unchanged complete source file.\nAll operations use the referenced before-image. Light permits replacement\nof lines not fully displayed, but rejects changed targets, missing history,\ninvalid paths and stale approval. Known unrelated edits can move an untouched\ntarget; the host maps them. Unknown edits require a new read. Never invent\na reference or choose a target by similar text. Empty PUT is invalid; use\nCUT. A valid no-op is not an error or proof of task completion. Pre-commit\nrefusal writes no target. Inspect commit I/O outcomes; never retry blindly.";

const HASHLINE_LIGHT_DESCRIPTION: &str = "Edit files from a read/search snapshot. Copy its [path@reference] header.\nPUT N: or PUT N-M: replaces original lines using + body rows. CUT N or\nCUT N-M removes them. PUT <N:, PUT >N:, and PUT >$: insert in original\nsnapshot gaps.\n[PATH#NEW] plus PUT >$: creates a file. REM explicitly deletes; MV DEST\nrenames after edits. REM/MV require the unchanged complete source file.\nAll operations use the referenced before-image. Light permits replacement\nof lines not fully displayed, but rejects changed targets, missing history,\ninvalid paths and stale approval. Known unrelated edits can move an untouched\ntarget; the host maps them. Unknown edits require a new read. Never invent\na reference or choose a target by similar text. Empty PUT is invalid; use\nCUT. A valid no-op is not an error or proof of task completion. Pre-commit\nrefusal writes no target. Inspect commit I/O outcomes; never retry blindly.";

const HASHLINE_ENHANCED_DESCRIPTION_SYMBOLS: &str = "Edit files from a read/search snapshot. Copy its [path@reference] header.\nPUT N: or PUT N-M: replaces original lines using + body rows. CUT removes\noriginal lines. PUT <N:, PUT >N:, and PUT >$: insert in source-snapshot\ngaps. N* operations require enabled symbol support. Every existing cell\nchanged by replacement or CUT must have been fully shown to this caller.\nFolded/truncated rows do not count. A missing-context reply can supply rows\nfor a later request, never this one. The host maps intact targets through\nknown edits. Equal text elsewhere is not the target; missing history needs\na fresh read. Known changed targets stay invalid even if old text returns.\n[PATH#NEW] creates. REM explicitly deletes under unchanged full-file\nreference and approval; REM is exempt from the every-line display rule.\nMV DEST renames after edits. Do not mix references or overlap operations.\nA valid no-op is not an error or proof of task completion. Never downgrade\nto Light to bypass refusal. Never blind-retry an uncertain commit.";

const HASHLINE_ENHANCED_DESCRIPTION: &str = "Edit files from a read/search snapshot. Copy its [path@reference] header.\nPUT N: or PUT N-M: replaces original lines using + body rows. CUT removes\noriginal lines. PUT <N:, PUT >N:, and PUT >$: insert in source-snapshot\ngaps. Every existing cell\nchanged by replacement or CUT must have been fully shown to this caller.\nFolded/truncated rows do not count. A missing-context reply can supply rows\nfor a later request, never this one. The host maps intact targets through\nknown edits. Equal text elsewhere is not the target; missing history needs\na fresh read. Known changed targets stay invalid even if old text returns.\n[PATH#NEW] creates. REM explicitly deletes under unchanged full-file\nreference and approval; REM is exempt from the every-line display rule.\nMV DEST renames after edits. Do not mix references or overlap operations.\nA valid no-op is not an error or proof of task completion. Never downgrade\nto Light to bypass refusal. Never blind-retry an uncertain commit.";

const APPLY_PATCH_DESCRIPTION: &str = "The `patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.";
