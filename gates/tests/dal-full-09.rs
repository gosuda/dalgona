//! Gate ledgers verify manifest targets and reject private planning paths or row citations.
#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test inspects tracked public content"
)]

use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Command,
};

const INVENTORIES: [(&str, &[&str]); 8] = [
    (
        "loop-headless",
        &[
            "loop-headless-01",
            "loop-headless-02",
            "loop-headless-03",
            "loop-headless-04",
            "loop-headless-05",
            "loop-headless-06",
            "loop-headless-07",
            "loop-headless-08",
            "loop-headless-09",
        ],
    ),
    (
        "loop-tui",
        &[
            "loop-tui-01",
            "loop-tui-02",
            "loop-tui-03",
            "loop-tui-04",
            "loop-tui-05",
            "loop-tui-06",
            "loop-tui-07",
            "loop-tui-08",
            "loop-tui-09",
            "loop-tui-10",
            "loop-tui-11",
            "loop-tui-12",
            "loop-tui-13",
        ],
    ),
    (
        "loop-plugin",
        &[
            "loop-plugin-01",
            "loop-plugin-02",
            "loop-plugin-03",
            "loop-plugin-04",
            "loop-plugin-05",
            "loop-plugin-06",
            "loop-plugin-07",
            "loop-plugin-08",
            "loop-plugin-09",
            "loop-plugin-10",
            "loop-plugin-11",
            "loop-plugin-12",
            "loop-plugin-13",
        ],
    ),
    ("soak", &["soak-01"]),
    (
        "wires",
        &[
            "wires-01", "wires-02", "wires-03", "wires-04", "wires-05", "wires-06", "wires-07",
            "wires-08",
        ],
    ),
    (
        "dal-full",
        &[
            "dal-full-01",
            "dal-full-02",
            "dal-full-03",
            "dal-full-04",
            "dal-full-05",
            "dal-full-06",
            "dal-full-07",
            "dal-full-08",
            "dal-full-09",
        ],
    ),
    ("login", &["login-cli-pty-01", "login-tui-01"]),
    ("guard-spill", &["guard-spill-01"]),
];

const RELEASE_TARGETS: [&str; 4] = [
    "release_publish_order",
    "release_guards",
    "release_semver",
    "release_dist",
];

#[test]
fn all_gate_ledgers_match_public_targets_and_pending_state()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let manifest = std::fs::read_to_string(root.join("gates/Cargo.toml"))?;
    let expected = INVENTORIES
        .iter()
        .flat_map(|(_, targets)| targets.iter().copied())
        .chain(RELEASE_TARGETS)
        .collect::<std::collections::BTreeSet<_>>();
    let actual = manifest_test_names(&manifest);
    assert_eq!(
        actual, expected,
        "every explicit target must have one source"
    );

    let private_path = ["dalgona-", "private"].concat();
    assert!(
        !manifest.contains(&private_path),
        "the public gate manifest must not depend on a private path"
    );

    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
            "-z",
        ])
        .current_dir(&root)
        .output()?;
    assert!(output.status.success(), "git ls-files failed");
    for relative in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(relative)?;
        let path = root.join(relative);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        assert_public_content_is_private_free(relative.as_bytes(), &path, &bytes);
    }
    Ok(())
}

fn manifest_test_names(manifest: &str) -> std::collections::BTreeSet<&str> {
    let mut names = std::collections::BTreeSet::new();
    let mut in_test = false;
    for line in manifest.lines() {
        if line.trim() == "[[test]]" {
            in_test = true;
        } else if in_test && line.trim_start().starts_with("name = ") {
            let value = line.trim().strip_prefix("name = ").unwrap();
            names.insert(value.trim_matches('"'));
            in_test = false;
        }
    }
    names
}

fn assert_public_content_is_private_free(path: &[u8], display_path: &Path, content: &[u8]) {
    let display_path = display_path.display();
    let local_prefix = ["local:", "/"].concat();
    let planning_files = [
        ["dalgon-v0-", "plan.md"].concat(),
        ["dalgon-v0-", "decisions.md"].concat(),
        ["dalgon-v0-", "skeleton.md"].concat(),
        ["spec-", "dalgon-v0.md"].concat(),
        ["plan-parts/", "steps/"].concat(),
        ["diamond-", "whole-plan/"].concat(),
    ];

    assert!(
        !contains(path, local_prefix.as_bytes()),
        "private path in {display_path}"
    );
    assert!(
        !contains(content, local_prefix.as_bytes()),
        "private path in {display_path}"
    );
    for filename in planning_files {
        assert!(
            !contains(path, filename.as_bytes()),
            "planning filename in {display_path}"
        );
        assert!(
            !contains(content, filename.as_bytes()),
            "planning filename in {display_path}"
        );
    }
    assert!(!has_row_number(path), "planning citation in {display_path}");
    assert!(
        !has_row_number(content),
        "planning citation in {display_path}"
    );
    // Round-9 citations arrived through these extra tokens. Each check stays
    // byte-oriented like `contains`/`has_row_number`, and every token literal
    // here is split so this file never carries a live one.
    let ignored_dir = [".out", "line/"].concat();
    let lane_dir = ["sd", "d/"].concat();
    // The ignored-dir rule itself lives in ignore files, so only they may
    // carry it.
    if !ends_with(path, b".gitignore") {
        assert!(
            !contains(path, ignored_dir.as_bytes()),
            "private path in {display_path}"
        );
        assert!(
            !contains(content, ignored_dir.as_bytes()),
            "private path in {display_path}"
        );
    }
    assert!(
        !contains(path, lane_dir.as_bytes()),
        "planning filename in {display_path}"
    );
    assert!(
        !contains(content, lane_dir.as_bytes()),
        "planning filename in {display_path}"
    );
    assert!(!has_report_doc(path), "planning filename in {display_path}");
    assert!(
        !has_report_doc(content),
        "planning filename in {display_path}"
    );
    let word_tokens = [
        ["PA", "DD"].concat(),
        ["VO", "C"].concat(),
        ["FL", "D"].concat(),
    ];
    for token in &word_tokens {
        assert!(
            !contains_word(path, token.as_bytes()),
            "planning token in {display_path}"
        );
        assert!(
            !contains_word(content, token.as_bytes()),
            "planning token in {display_path}"
        );
    }
    assert!(
        !has_eng_citation(path),
        "planning citation in {display_path}"
    );
    assert!(
        !has_eng_citation(content),
        "planning citation in {display_path}"
    );
    assert!(
        !has_req_citation(path),
        "planning citation in {display_path}"
    );
    assert!(
        !has_req_citation(content),
        "planning citation in {display_path}"
    );
    assert!(!has_part_token(path), "planning citation in {display_path}");
    assert!(
        !has_part_token(content),
        "planning citation in {display_path}"
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn has_row_number(bytes: &[u8]) -> bool {
    let prefix = *b"row ";
    for (index, window) in bytes.windows(prefix.len()).enumerate() {
        if !window.eq_ignore_ascii_case(&prefix) {
            continue;
        }
        // A citation token cannot start inside a longer word: model names
        // such as "Arrow X.Y" carry the prefix plus a digit inside them.
        if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let start = index + prefix.len();
        let mut end = start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start || matches!(bytes.get(end), Some(b'x' | b'X')) {
            continue;
        }
        if cites_continuation(bytes, end) {
            return true;
        }
        if cites_with_verb(bytes, index) {
            return true;
        }
        if is_terminal_coordinate(bytes, index) {
            continue;
        }
        return true;
    }
    false
}

fn cites_continuation(bytes: &[u8], end: usize) -> bool {
    matches!(bytes.get(end), Some(b'/' | b'-' | b':'))
}

fn cites_with_verb(bytes: &[u8], index: usize) -> bool {
    const VERBS: [&[u8]; 7] = [
        b"refer",
        b"refers",
        b"reference",
        b"references",
        b"see",
        b"specified",
        b"defined",
    ];
    const WINDOW: usize = 32;
    let from = index.saturating_sub(WINDOW);
    VERBS
        .iter()
        .any(|verb| contains_word(&bytes[from..index], verb))
}

fn is_terminal_coordinate(bytes: &[u8], index: usize) -> bool {
    const HINTS: [&[u8]; 10] = [
        b"art",
        b"screen",
        b"caret",
        b"block",
        b"terminal",
        b"frame",
        b"cell",
        b"cursor",
        b"scroll",
        b"column",
    ];
    const WINDOW: usize = 32;
    if !preceded_by_locator(bytes, index) {
        return false;
    }
    let from = index.saturating_sub(WINDOW);
    HINTS
        .iter()
        .any(|hint| contains_word(&bytes[from..index], hint))
}

fn preceded_by_locator(bytes: &[u8], index: usize) -> bool {
    const LOCATORS: [&[u8]; 2] = [b"at ", b"to "];
    LOCATORS.iter().any(|locator| {
        index >= locator.len()
            && bytes[index - locator.len()..index].eq_ignore_ascii_case(locator)
            && (index == locator.len() || !bytes[index - locator.len() - 1].is_ascii_alphanumeric())
    })
}

fn contains_word(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .any(|(at, window)| {
            window.eq_ignore_ascii_case(needle)
                && (at == 0 || !haystack[at - 1].is_ascii_alphanumeric())
                && (at + needle.len() == haystack.len()
                    || !haystack[at + needle.len()].is_ascii_alphanumeric())
        })
}

fn ends_with(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && &haystack[haystack.len() - needle.len()..] == needle
}

fn has_report_doc(bytes: &[u8]) -> bool {
    // A report doc is the marker plus a lowercase dashed name ending in the
    // doc suffix; bare prose without the suffix stays public.
    let prefix: &[u8] = b"report-";
    let suffix: &[u8] = b".md";
    for (index, window) in bytes.windows(prefix.len()).enumerate() {
        if window != prefix {
            continue;
        }
        if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut end = index + prefix.len();
        while bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_lowercase() || *byte == b'-')
        {
            end += 1;
        }
        if end == index + prefix.len() {
            continue;
        }
        if bytes.get(end..end + suffix.len()) == Some(suffix) {
            return true;
        }
    }
    false
}

fn has_upper_tag(bytes: &[u8], tag: &[u8]) -> bool {
    // Tags stay uppercase-only so lowercase product ids never match; the
    // numbered-prefix shape shares the same tag core.
    if tag.is_empty() || bytes.len() < tag.len() {
        return false;
    }
    bytes.windows(tag.len()).enumerate().any(|(at, window)| {
        window == tag
            && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric())
            && bytes.get(at + tag.len()).is_some_and(u8::is_ascii_digit)
    })
}

fn has_eng_citation(bytes: &[u8]) -> bool {
    has_upper_tag(bytes, b"ENG-")
}

fn has_req_citation(bytes: &[u8]) -> bool {
    has_upper_tag(bytes, b"REQ-")
}

fn has_part_token(bytes: &[u8]) -> bool {
    let word: &[u8] = b"part";
    if bytes.len() < word.len() {
        return false;
    }
    bytes.windows(word.len()).enumerate().any(|(at, window)| {
        window.eq_ignore_ascii_case(word)
            && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric())
            && matches!(bytes.get(at + word.len()), Some(b' ' | b'-'))
            && bytes
                .get(at + word.len() + 1)
                .is_some_and(u8::is_ascii_digit)
            && !is_journal_coordinate(bytes, at + word.len() + 1)
    })
}

fn is_journal_coordinate(bytes: &[u8], digits_at: usize) -> bool {
    // Journal dumps render a count plus a byte range; those coordinates are
    // public format, not plan citations.
    let marker: &[u8] = b"bytes";
    let mut end = digits_at;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    let mut rest = &bytes[end..];
    if rest.first() == Some(&b',') {
        rest = &rest[1..];
    }
    while rest.first() == Some(&b' ') {
        rest = &rest[1..];
    }
    rest.len() >= marker.len() && rest[..marker.len()].eq_ignore_ascii_case(marker)
}

#[test]
fn row_citation_check_tells_citations_from_terminal_coordinates() {
    let per = ["per row ", "128"].concat();
    assert!(has_row_number(per.as_bytes()));
    let capital = ["Row ", "131/132 and fork U-1"].concat();
    assert!(has_row_number(capital.as_bytes()));
    let by = ["closed by row ", "132"].concat();
    assert!(has_row_number(by.as_bytes()));
    let paren = ["planned (row ", "124)"].concat();
    assert!(has_row_number(paren.as_bytes()));
    let glued = ["that row ", "90 never lists"].concat();
    assert!(has_row_number(glued.as_bytes()));
    let refer_to = ["refer to row ", "128"].concat();
    assert!(has_row_number(refer_to.as_bytes()));
    let specified_at = ["specified at row ", "128"].concat();
    assert!(has_row_number(specified_at.as_bytes()));
    let see_form = ["see row ", "9 for the shape"].concat();
    assert!(has_row_number(see_form.as_bytes()));
    let colon_site = ["amend the sites to row ", "96: placeholders"].concat();
    assert!(has_row_number(colon_site.as_bytes()));
    let art = ["shifts the art to row ", "1"].concat();
    assert!(!has_row_number(art.as_bytes()));
    let screen = ["scrolled off the screen starts at row ", "1"].concat();
    assert!(!has_row_number(screen.as_bytes()));
    let model = ["Narrow ", "5 wide"].concat();
    assert!(!has_row_number(model.as_bytes()));
    let dims = ["a row ", "80x24 grid"].concat();
    assert!(!has_row_number(dims.as_bytes()));
}

#[test]
fn private_token_checks_catch_each_leak_class() {
    // Every literal stays split so this file never carries a live token.
    let ignored = [".out", "line/notes"].concat();
    assert!(contains(
        ignored.as_bytes(),
        [".out", "line/"].concat().as_bytes()
    ));

    let lane = ["merge sd", "d/plan"].concat();
    assert!(contains(lane.as_bytes(), ["sd", "d/"].concat().as_bytes()));

    let report_doc = ["nightly report-", "ops-summary.md"].concat();
    assert!(has_report_doc(report_doc.as_bytes()));

    let brief = ["uses PA", "DD here"].concat();
    assert!(contains_word(
        brief.as_bytes(),
        ["PA", "DD"].concat().as_bytes()
    ));
    let spoken = ["says VO", "C aloud"].concat();
    assert!(contains_word(
        spoken.as_bytes(),
        ["VO", "C"].concat().as_bytes()
    ));
    let field = ["marks FL", "D now"].concat();
    assert!(contains_word(
        field.as_bytes(),
        ["FL", "D"].concat().as_bytes()
    ));

    let build = ["tracks EN", "G-12"].concat();
    assert!(has_eng_citation(build.as_bytes()));
    let requirement = ["pins RE", "Q-39-14"].concat();
    assert!(has_req_citation(requirement.as_bytes()));
    let numbered = ["spec 36-RE", "Q-11 done"].concat();
    assert!(has_req_citation(numbered.as_bytes()));
    let part_shape = ["see (part ", "39) below"].concat();
    assert!(has_part_token(part_shape.as_bytes()));
}

#[test]
fn private_token_checks_allow_public_prose() {
    let padding = ["PAD", "DING"].concat();
    let word_tokens = [
        ["PA", "DD"].concat(),
        ["VO", "C"].concat(),
        ["FL", "D"].concat(),
    ];
    for token in &word_tokens {
        assert!(!contains_word(padding.as_bytes(), token.as_bytes()));
    }

    let prose = "the broker holds the question open until the deadline";
    assert!(!has_report_doc(prose.as_bytes()));
    assert!(!has_eng_citation(prose.as_bytes()));
    assert!(!has_req_citation(prose.as_bytes()));
    assert!(!has_part_token(prose.as_bytes()));
    assert!(!has_row_number(prose.as_bytes()));

    // Lowercase product ids are not requirement tags.
    let request_id = ["request id ", "req-1"].concat();
    assert!(!has_req_citation(request_id.as_bytes()));

    // Journal coordinates stay public.
    let journal = ["entry 2, user, part ", "0, bytes 0-"].concat();
    assert!(!has_part_token(journal.as_bytes()));

    // The prose marker without a doc suffix stays public.
    let report_only = "three report-only detectors";
    assert!(!has_report_doc(report_only.as_bytes()));
}

#[test]
fn ignore_files_may_carry_the_ignored_dir_rule() {
    let rule = [".out", "line/"].concat();
    assert_public_content_is_private_free(b".gitignore", Path::new(".gitignore"), rule.as_bytes());
}

#[test]
#[should_panic(expected = "private path")]
fn public_content_rejects_the_ignored_dir_elsewhere() {
    let rule = [".out", "line/notes"].concat();
    assert_public_content_is_private_free(
        b"docs/notes.md",
        Path::new("docs/notes.md"),
        rule.as_bytes(),
    );
}
