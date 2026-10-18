//! Docs-truth suite: generator, resolver, and manual truth checks.
#![expect(
    clippy::unwrap_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]

use dal_ext::docs::{
    DocsSnapshot, Lookup, Manual, Miss, listing, lookup, miss_lines, nearest, page_valid,
    read_miss_line, render_index, scheme_valid, snapshot as live_snapshot, wire_error,
};
use dal_ext::docsgen::{GenArgs, Scheme, generate};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scan_roots() -> Vec<PathBuf> {
    [
        "core",
        "engine",
        "extensions",
        "clients",
        "cli",
        "docs",
        "examples/plugins",
    ]
    .into_iter()
    .map(|path| repo_root().join(path))
    .collect()
}

fn dal_generation_args() -> GenArgs {
    GenArgs {
        scheme: Scheme::Dal,
        dir: docs_dir(),
        examples: Some(examples_dir()),
        scan: scan_roots(),
    }
}

const SCHEME: &str = "dal";
const DGSCHEME: &str = "dalgona";

/// Assembles a URI from parts: the generator scans test sources, so
/// deliberate-miss URIs must never appear as `scheme://page` literals.
fn u(page: &str) -> String {
    format!("{SCHEME}://{page}")
}

fn docs_dir() -> PathBuf {
    repo_root().join("docs")
}

fn examples_dir() -> PathBuf {
    repo_root().join("examples/plugins")
}

fn load_manual() -> BTreeMap<String, String> {
    live_snapshot()
        .manuals
        .into_iter()
        .next()
        .map(|manual| manual.pages.into_iter().collect())
        .unwrap_or_default()
}

fn snapshot() -> DocsSnapshot {
    live_snapshot()
}

fn fixture(names: &[(&str, &str)]) -> (tempfile_guard::Guard, GenArgs) {
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    let mut list = String::new();
    for (name, text) in names {
        if name.contains('/') {
            continue;
        }
        std::fs::write(dir.join(format!("{name}.md")), text).unwrap();
    }
    for (name, _) in names {
        let _ = writeln!(list, "{name} user docs 100");
    }
    std::fs::write(dir.join("pages"), list).unwrap();
    let scan = dir.clone();
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: None,
        scan: vec![scan],
    };
    let _ = &dir;
    (guard, args)
}

mod tempfile_guard {
    use std::path::PathBuf;
    pub(super) struct Guard {
        dir: PathBuf,
    }
    impl Guard {
        pub(super) fn new() -> Self {
            // Clock resolution alone can collide on hosts whose timer is
            // coarser than as_nanos implies (two tests in the same tick
            // would share a dir and cross-write pages), so a per-process
            // counter keeps every guard unique.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "dal-docs-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
        pub(super) fn dir(&self) -> &PathBuf {
            &self.dir
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[test]
fn generator_two_pages_deterministic() {
    let (_guard, args) = fixture(&[
        ("b", "# Bee\n\nSecond page.\n"),
        ("a", "# Aye\n\nFirst page.\n"),
    ]);
    let first = generate(&args).expect("generates");
    let second = generate(&args).expect("generates");
    assert_eq!(first, second, "two runs give identical bytes");
    assert!(first.contains("\"a\""), "module holds page a");
    assert!(first.contains("\"b\""), "module holds page b");
    assert!(first.find("\"a\"") < first.find("\"b\""), "pages sorted");
}

#[test]
fn generated_manual_is_fresh() {
    let expected = generate(&dal_generation_args()).expect("generates dal manual");
    let actual = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/docs_generated.rs"),
    )
    .expect("generated manual source");
    assert_eq!(
        actual, expected,
        "regenerate docs_generated.rs with docs-gen"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "bootstrap compiles and runs the real generator without library artifacts"
)]
fn generator_bootstraps_without_generated_manual_or_library() -> std::io::Result<()> {
    let (guard, args) = fixture(&[("bootstrap", "# Bootstrap\n\nGenerate from a clean tree.\n")]);
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let isolated_source = guard.dir().join("src");
    std::fs::create_dir_all(isolated_source.join("bin"))?;
    for file in ["bin/docs-gen.rs", "docsgen.rs"] {
        std::fs::copy(source.join(file), isolated_source.join(file))?;
    }
    let executable = guard
        .dir()
        .join(format!("docs-gen{}", std::env::consts::EXE_SUFFIX));
    let compiled = std::process::Command::new("rustc")
        .arg("--edition=2024")
        .arg(isolated_source.join("bin/docs-gen.rs"))
        .arg("-o")
        .arg(&executable)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()?;
    assert!(
        compiled.status.success(),
        "standalone generator compilation failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = guard.dir().join("docs_generated.rs");
    let generated = std::process::Command::new(&executable)
        .args(["--scheme", "dal", "--dir"])
        .arg(&args.dir)
        .arg("--scan")
        .arg(&args.dir)
        .arg("--out")
        .arg(&output)
        .current_dir(guard.dir())
        .output()?;
    assert!(
        generated.status.success(),
        "standalone generation failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(output)?,
        generate(&args).expect("fixture generates")
    );
    Ok(())
}

#[test]
fn generator_keeps_nested_product_pages_and_rejects_nested_dal_pages() {
    let (_guard, mut args) =
        fixture(&[("ttsr/rules", "# Rule reference\n\nPreserve this content.\n")]);
    std::fs::create_dir_all(args.dir.join("ttsr")).unwrap();
    std::fs::write(
        args.dir.join("ttsr/rules.md"),
        "# Rule reference\n\nPreserve this content.\n",
    )
    .unwrap();
    assert!(generate(&args).is_err());
    args.scheme = Scheme::Dalgona;
    let generated = generate(&args).unwrap();
    assert!(generated.contains("\"ttsr/rules\""));
    assert!(generated.contains("Preserve this content."));
}

#[test]
fn generator_missing_page_error() {
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pages"), "b user docs 100\n").unwrap();
    let list = dir.join("pages");
    let missing = dir.join("b.md");
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: None,
        scan: vec![dir],
    };
    let errors = generate(&args).expect_err("missing page fails");
    assert_eq!(
        errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec![format!(
            "dal-docs: {} lists {}://b, but {} does not exist.",
            list.display(),
            SCHEME,
            missing.display()
        )],
    );
}

#[test]
fn generator_unlisted_page_error() {
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pages"), "a user docs 100\n").unwrap();
    std::fs::write(dir.join("a.md"), "# Aye\n").unwrap();
    std::fs::write(dir.join("c.md"), "# Sea\n").unwrap();
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: None,
        scan: vec![dir],
    };
    let errors = generate(&args).expect_err("unlisted page fails");
    assert!(
        errors.iter().any(|e| e.to_string().contains("not listed")),
        "unlisted message, got {errors:?}"
    );
}

#[test]
fn generator_title_error() {
    let (_guard, args) = fixture(&[("a", "No title here\n")]);
    let errors = generate(&args).expect_err("title fails");
    assert!(
        errors
            .iter()
            .any(|e| e.to_string().contains("first line must be")),
        "title error, got {errors:?}"
    );
}

#[test]
fn generator_size_error() {
    let big = format!("# Big\n\n{}", "x".repeat(49160));
    let (_guard, args) = fixture(&[("a", &big)]);
    let errors = generate(&args).expect_err("size fails");
    assert!(
        errors.iter().any(|e| e.to_string().contains("49152")),
        "size error, got {errors:?}"
    );
}

#[test]
fn generator_budget_and_line_errors() {
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    let over_lines = (0..1502)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("# Big\n\n{over_lines}\n");
    std::fs::write(dir.join("a.md"), &text).unwrap();
    std::fs::write(dir.join("pages"), "a user docs 100\n").unwrap();
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: None,
        scan: vec![dir],
    };
    let errors = generate(&args).expect_err("budget fails");
    assert!(errors.len() >= 2, "both limit messages, got {errors:?}");
}

#[test]
fn generator_utf8_error() {
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pages"), "a user docs 100\n").unwrap();
    std::fs::write(dir.join("a.md"), [b'#', b' ', 0xff, b'\n']).unwrap();
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: None,
        scan: vec![dir],
    };
    let errors = generate(&args).expect_err("utf8 fails");
    assert!(
        errors
            .iter()
            .any(|e| e.to_string().contains("not valid UTF-8")),
        "utf8 message, got {errors:?}"
    );
}

#[test]
fn generator_link_errors() {
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    let source = guard.dir().join("references.rs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pages"), "a user docs 100\n").unwrap();
    std::fs::write(dir.join("a.md"), "# Aye\n").unwrap();
    std::fs::write(
        &source,
        format!(
            r#"const _: &str = "{SCHEME}://a.\n {SCHEME}://a.\r {SCHEME}://a.\t {SCHEME}://a.\" {SCHEME}://a.\'";
"#
        ),
    )
    .unwrap();
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: None,
        scan: vec![guard.dir().to_path_buf()],
    };
    generate(&args).expect("valid source delimiters end the URI");

    std::fs::write(&source, format!(r#"const _: &str = "{SCHEME}://a\q.";"#)).unwrap();
    let malformed = generate(&args).expect_err("malformed escape stays in the URI");
    assert!(
        malformed
            .iter()
            .any(|e| e.to_string().contains("is not a page")),
        "malformed link error, got {malformed:?}"
    );

    std::fs::write(&source, "const _: &str = \"no links\";\n").unwrap();
    std::fs::write(
        dir.join("a.md"),
        format!("# Aye\n\nSee {SCHEME}://missing-page.\n"),
    )
    .unwrap();
    let errors = generate(&args).expect_err("link fails");
    assert!(
        errors
            .iter()
            .any(|e| e.to_string().contains("is not a page")),
        "link error, got {errors:?}"
    );
}

#[test]
fn generator_list_errors() {
    for list in [
        "a user\n",
        "Bad user docs 100\n",
        "a stranger docs 100\n",
        "a user docs 100\na user docs 100\n",
        "a user docs 0\n",
        "a user docs 1501\n",
        "a user nowhere 100\n",
    ] {
        let guard = tempfile_guard::Guard::new();
        let dir = guard.dir().join("docs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pages"), list).unwrap();
        let args = GenArgs {
            scheme: Scheme::Dal,
            dir: dir.clone(),
            examples: None,
            scan: vec![dir],
        };
        assert!(generate(&args).is_err(), "list rule fails for {list:?}");
    }
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pages"), "\n\n").unwrap();
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: None,
        scan: vec![dir],
    };
    assert!(generate(&args).is_err(), "empty list fails");
}

#[test]
fn generator_example_assembly() {
    let guard = tempfile_guard::Guard::new();
    let dir = guard.dir().join("docs");
    let examples = guard.dir().join("plugins");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(examples.join("demo")).unwrap();
    std::fs::write(dir.join("pages"), "examples/demo user docs 400\n").unwrap();
    std::fs::write(examples.join("demo/README.md"), "# demo\n\nDemo port.\n").unwrap();
    std::fs::write(
        examples.join("demo/plugin.star"),
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"demo\", version = \"0.1.0\")\n",
    )
    .unwrap();
    std::fs::write(examples.join("demo/extra.star"), "x = 1\n").unwrap();
    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: dir.clone(),
        examples: Some(examples),
        scan: vec![dir],
    };
    let module = generate(&args).expect("assembles");
    assert!(module.contains("## Files"), "page holds files section");
    assert!(module.contains("starlark"), "nested file fenced with tag");
}

#[test]
fn resolver_index_and_page_bytes() {
    let snap = snapshot();
    match lookup(&snap, "dal://") {
        Lookup::Index { uri, text } => {
            assert_eq!(uri, "dal://");
            let manual = &snap.manuals[0];
            assert_eq!(text, render_index(manual));
        }
        other => panic!("index expected, got {other:?}"),
    }
    match lookup(&snap, "dal://config") {
        Lookup::Page { uri, title, text } => {
            assert_eq!(uri, "dal://config");
            assert_ne!(title, "");
            assert!(text.starts_with("# "));
        }
        other => panic!("page expected, got {other:?}"),
    }
}

#[test]
fn resolver_page_miss_nearest() {
    let snap = snapshot();
    match lookup(&snap, &u("confg")) {
        Lookup::Miss(Miss::NoPage { nearest, .. }) => {
            assert_eq!(nearest.as_deref(), Some("dal://config"));
        }
        other => panic!("miss expected, got {other:?}"),
    }
    let lines = miss_lines(
        &Miss::NoPage {
            scheme: "dal".to_owned(),
            uri: u("confg"),
            nearest: Some(u("config")),
        },
        "dalgon",
    );
    assert_eq!(
        lines[0],
        format!("dalgon: no dal document at {}", u("confg"))
    );
    assert_eq!(
        lines[1],
        format!(
            "Did you mean {}? Run dalgon docs for the list.",
            u("config")
        )
    );
}

#[test]
fn resolver_not_a_uri() {
    let snap = snapshot();
    match lookup(&snap, "config") {
        Lookup::Miss(Miss::NotAUri { nearest, .. }) => {
            assert_eq!(nearest.as_deref(), Some("dal://config"));
        }
        other => panic!("miss expected, got {other:?}"),
    }
    match lookup(&snap, "zzz") {
        Lookup::Miss(Miss::NotAUri { nearest, .. }) => {
            assert_eq!(nearest, None);
        }
        other => panic!("miss expected, got {other:?}"),
    }
}

#[test]
fn resolver_no_scheme() {
    let snap = DocsSnapshot {
        manuals: vec![
            Manual {
                scheme: "dal".to_owned(),
                plugin: "dal".to_owned(),
                pages: vec![],
            },
            Manual {
                scheme: "dalgona".to_owned(),
                plugin: "dalgona".to_owned(),
                pages: vec![],
            },
        ],
    };
    match lookup(&snap, "foo://x") {
        Lookup::Miss(Miss::NoScheme { scheme, known }) => {
            assert_eq!(scheme, "foo");
            assert_eq!(known, vec!["dal".to_owned(), "dalgona".to_owned()]);
        }
        other => panic!("miss expected, got {other:?}"),
    }
}

#[test]
fn resolver_grammar_edges() {
    let snap = snapshot();
    for uri in [u("../x"), u("CLI"), u("a/"), u("a//b")] {
        match lookup(&snap, uri.as_str()) {
            Lookup::Miss(Miss::NoPage { .. }) => {}
            other => panic!("{uri} must be NoPage, got {other:?}"),
        }
    }
}

#[test]
fn prop_index_column() {
    let snap = snapshot();
    for manual in &snap.manuals {
        let text = render_index(manual);
        let mut columns = std::collections::HashSet::new();
        for line in text.lines().skip(1) {
            let title_start = line.find(|c: char| c != ' ').unwrap_or(line.len());
            let _ = title_start;
            let trimmed = line.trim_start();
            let first_space = trimmed.find(' ').unwrap_or(trimmed.len());
            columns.insert(trimmed[..first_space].len());
        }
        assert!(columns.iter().next().is_some());
    }
}

#[test]
fn prop_lookup_total() {
    let snap = snapshot();
    let abc = u("a/b/c");
    for text in [
        "",
        "://",
        "dal://",
        "DALGON://CONFIG",
        abc.as_str(),
        "\u{1f600}",
        "a".repeat(300).as_str(),
    ] {
        let _ = lookup(&snap, text);
    }
}

#[test]
fn prop_page_valid_regex() {
    let re = regex::Regex::new(r"^[a-z0-9][a-z0-9-]*(/[a-z0-9][a-z0-9-]*)*$").unwrap();
    for page in [
        "config",
        "a-b",
        "a/b-c",
        "0x",
        "examples/todo",
        "",
        "A",
        "a/",
        "a//b",
        "a_b",
    ] {
        assert_eq!(page_valid(page), re.is_match(page), "page {page:?}");
    }
    assert!(scheme_valid("dal"));
    assert!(!scheme_valid("Dalgon"));
    assert!(!scheme_valid(""));
}

#[test]
fn prop_nearest_minimal() {
    // Bare page names, sorted per the contract; production pairs full URIs
    // with pages itself in `lookup`.
    let candidates = ["cli", "config", "context"];
    let borrowed: Vec<&str> = candidates.to_vec();
    assert_eq!(nearest(&borrowed, "config"), Some("config".to_owned()));
    assert_eq!(nearest(&borrowed, "zzz"), None);
}

#[test]
fn read_tool_doc_miss() {
    let miss = Miss::NoPage {
        scheme: "dal".to_owned(),
        uri: u("confg"),
        nearest: Some(u("config")),
    };
    assert_eq!(
        read_miss_line(&miss),
        Some(format!(
            "read: {} does not exist. Did you mean {}? Read dal:// for the index.",
            u("confg"),
            u("config")
        ))
    );
    let snap = snapshot();
    if let Lookup::Index { text, .. } = lookup(&snap, "dal://") {
        assert_eq!(text, listing(&snap));
    } else {
        panic!("index expected");
    }
}

#[test]
fn wire_docs_read() {
    let miss = Miss::NoPage {
        scheme: "dal".to_owned(),
        uri: u("confg"),
        nearest: Some(u("config")),
    };
    assert_eq!(
        wire_error(&miss),
        (
            -32002,
            format!("no document at {}", u("confg")),
            Some(format!("Did you mean {}?", u("config")))
        )
    );
    let miss = Miss::NotAUri {
        text: "config".to_owned(),
        nearest: None,
    };
    assert_eq!(wire_error(&miss).0, -32602);
}

#[test]
fn truth_config_page() {
    use std::fmt::Write as _;

    use dal_core::{Config, ConfigProduct};

    let pages = load_manual();
    let config = pages.get("config").expect("config page");
    let mut scalar_toml = String::new();
    let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in config.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 6, "key/type/default/meaning columns: {line}");
        let (key, kind, default) = (cells[1], cells[2], cells[3]);
        let name = key.trim_matches('`');
        let value = default.trim_matches('`');
        if value == "none" || name.contains("<model-id>") {
            continue;
        }
        let literal = if kind.trim_matches('`') == "string" && !value.starts_with('"') {
            format!("\"{value}\"")
        } else {
            value.to_owned()
        };
        match name.split_once('.') {
            None => {
                let _ = writeln!(scalar_toml, "{name} = {literal}");
            }
            Some((section, rest)) => sections
                .entry(section.to_owned())
                .or_default()
                .push(format!("{rest} = {literal}")),
        }
    }
    let mut document = scalar_toml;
    for (section, entries) in &sections {
        let _ = writeln!(document, "[{section}]\n{}", entries.join("\n"));
    }
    let root = Path::new("<data-root>");
    let defaults = Config::load(ConfigProduct::Dalgon, root, "", None).expect("built-in defaults");
    let loaded =
        Config::load(ConfigProduct::Dalgon, root, "", Some(&document)).expect("page defaults load");
    assert_eq!(loaded.mode(), defaults.mode());
    assert_eq!(loaded.model(), defaults.model());
    assert_eq!(loaded.thinking(), defaults.thinking());
    assert_eq!(loaded.approval(), defaults.approval());
    assert_eq!(loaded.screen(), defaults.screen());
    assert_eq!(loaded.theme(), defaults.theme());
    assert_eq!(loaded.sandbox(), defaults.sandbox());
    assert_eq!(loaded.sandbox_writable(), defaults.sandbox_writable());
    assert_eq!(loaded.images(), defaults.images());
    assert_eq!(loaded.motion(), defaults.motion());
    assert_eq!(
        loaded.compact_ratio().to_bits(),
        defaults.compact_ratio().to_bits()
    );
    assert_eq!(loaded.edit_style(), defaults.edit_style());
    assert_eq!(loaded.guard(), defaults.guard());
    assert_eq!(loaded.search_symbols(), defaults.search_symbols());
    assert_eq!(loaded.plugins(), defaults.plugins());
    assert_eq!(loaded.rules(), defaults.rules());
    assert_eq!(loaded.serve(), defaults.serve());
}

/// Splits a markdown table row on unescaped `|` and unescapes `\|`.
fn split_cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
        } else if ch == '|' {
            cells.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    cells.push(current);
    cells.iter().map(|cell| cell.trim().to_owned()).collect()
}

#[test]
fn truth_commands_page() {
    use dal_ext::commands::specs;
    let pages = load_manual();
    let commands = pages.get("commands").expect("commands page");
    let rows: Vec<Vec<String>> = commands
        .lines()
        .filter(|line| line.starts_with("| /"))
        .map(split_cells)
        .collect();
    let specs: Vec<_> = specs().collect();
    assert_eq!(rows.len(), specs.len(), "one row per registry entry");
    for (row, spec) in rows.iter().zip(specs.iter()) {
        assert_eq!(row[1], format!("/{}", spec.name), "name column");
        assert_eq!(row[2], spec.hint, "hint column for /{}", spec.name);
        assert_eq!(row[3], spec.summary, "summary column for /{}", spec.name);
    }
}

#[test]
fn truth_ext_status_claims() {
    let pages = load_manual();
    let protocol = pages.get("protocol").expect("protocol page");
    let cli = pages.get("cli").expect("cli page");
    assert!(protocol.contains("\"type\":\"ext_status\""));
    assert!(
        protocol
            .contains(r#"{"type":"ext_status","ext":"focus","state":"busy","text":"indexing"}"#)
    );
    assert!(protocol.contains(r#"{"type":"ext_status","ext":"focus","state":"quiet"}"#));
    assert!(protocol.contains("short fixed interval"));
    assert!(protocol.contains("sends an update only when the state or text changes"));
    assert!(protocol.contains("A quiet extension with no text is the starting state"));
    assert!(protocol.contains("A fresh wire subscription has no status seed"));
    assert!(cli.contains("waits up to 3 seconds"));
    assert!(cli.contains("exits 1 if a kind remains busy after the quiet wait"));
    assert!(cli.contains("JSON mode has no time limit"));

    let busy: dal_core::Update = sonic_rs::from_str(
        r#"{"gen":1,"seq":5,"kind":{"type":"ext_status","ext":"focus","state":"busy","text":"indexing","future":1}}"#,
    )
    .expect("busy ext_status decodes");
    let dal_core::UpdateKind::ExtStatus(status) = busy.kind else {
        panic!("busy ext_status update expected");
    };
    assert_eq!(status.ext.as_ref(), "focus");
    assert_eq!(status.state, dal_core::ExtState::Busy);
    assert_eq!(status.text.as_deref(), Some("indexing"));
    assert_eq!(status.to_string(), "focus: indexing");

    let unknown: dal_core::Update = sonic_rs::from_str(
        r#"{"gen":1,"seq":6,"kind":{"type":"ext_status","ext":"focus","state":"starting"}}"#,
    )
    .expect("unknown status state decodes tolerantly");
    assert!(matches!(unknown.kind, dal_core::UpdateKind::Unknown));

    let quiet = dal_core::ExtStatus {
        ext: "focus".into(),
        state: dal_core::ExtState::Quiet,
        text: None,
    };
    assert!(quiet.is_quiet());
    assert!(quiet.text.is_none());
    assert_eq!(quiet.to_string(), "focus: quiet");
    let quiet_wire = sonic_rs::to_string(&dal_core::UpdateKind::ExtStatus(quiet))
        .expect("quiet ext_status serializes");
    assert!(quiet_wire.contains(r#""state":"quiet""#));
}

#[test]
fn truth_toml_blocks() {
    use dal_core::{Config, ConfigProduct};
    let pages = load_manual();
    let dir = std::env::temp_dir().join("dal-docs-toml");
    let _ = std::fs::create_dir_all(&dir);
    let mut count = 0;
    for (page, text) in &pages {
        for block in fenced(text, "toml") {
            count += 1;
            Config::load(ConfigProduct::Dalgon, &dir, "", Some(&block))
                .unwrap_or_else(|error| panic!("toml block in {page} parses: {error}"));
        }
    }
    assert!(count > 0, "at least one toml block checked");
}

#[test]
fn truth_cross_manual_links() {
    let dal = load_manual();
    let dalgona_dir = repo_root().join("dalgona/docs");
    let mut dalgona_entries = Vec::new();
    for line in std::fs::read_to_string(dalgona_dir.join("pages"))
        .expect("dalgona pages")
        .lines()
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let text = std::fs::read_to_string(dalgona_dir.join(format!("{}.md", fields[0])))
            .expect("dalgona page");
        dalgona_entries.push((fields[0].to_owned(), text));
    }
    let dal_snap = DocsSnapshot {
        manuals: vec![Manual {
            scheme: "dal".to_owned(),
            plugin: "dal".to_owned(),
            pages: dal.into_iter().collect(),
        }],
    };
    let dg_snap = DocsSnapshot {
        manuals: vec![Manual {
            scheme: "dalgona".to_owned(),
            plugin: "dalgona".to_owned(),
            pages: dalgona_entries,
        }],
    };
    for manual in &dg_snap.manuals {
        for (_, text) in &manual.pages {
            for page in extract_links(text, "dal") {
                let uri = format!("{SCHEME}://{page}");
                match lookup(&dal_snap, &uri) {
                    Lookup::Page { .. } | Lookup::Index { .. } => {}
                    Lookup::Miss(miss) => panic!("{uri} resolves: {miss:?}"),
                }
            }
        }
    }

    let mut dg_to_dal = 0;
    for manual in &dal_snap.manuals {
        for (_, text) in &manual.pages {
            for page in extract_links(text, "dalgona") {
                let uri = format!("{DGSCHEME}://{page}");
                match lookup(&dg_snap, &uri) {
                    Lookup::Page { .. } | Lookup::Index { .. } => dg_to_dal += 1,
                    Lookup::Miss(miss) => panic!("{uri} resolves: {miss:?}"),
                }
            }
        }
    }
    let index_mentions = dg_snap
        .manuals
        .iter()
        .flat_map(|manual| &manual.pages)
        .filter(|(_, text)| text.contains("`dal://`"))
        .count();
    assert!(
        index_mentions > 0,
        "a dalgona page names the dal manual by its bare index URI"
    );
    assert!(
        matches!(lookup(&dal_snap, "dal://"), Lookup::Index { .. }),
        "the bare dal:// manual reference resolves as the dal index"
    );
    assert!(
        dg_to_dal > 0,
        "a dalgona:// link in a dal page resolves in dalgona"
    );
}

#[test]
fn docs_door_rendering() {
    // The CLI door renders through the resolver: sorted listing with no URI,
    // the page title first for a known URI, miss bytes for unknown URIs, and
    // no coupling to plugin load state (the snapshot carries plugin names as
    // data, so a broken plugin cannot change these bytes).
    let snap = snapshot();
    let listed = listing(&snap);
    let mut uris: Vec<&str> = snap.manuals[0]
        .pages
        .iter()
        .map(|(page, _)| page.as_str())
        .collect();
    uris.sort_unstable();
    let mut lines = listed.lines();
    assert_eq!(lines.next(), Some("# dal://"));
    for (line, page) in lines.zip(uris.iter()) {
        assert!(line.starts_with(&format!("{SCHEME}://{page}")), "{line}");
    }
    match lookup(&snap, "dal://config") {
        Lookup::Page { title, .. } => assert_eq!(title, "Settings in dal.toml"),
        other => panic!("page expected, got {other:?}"),
    }
    let miss = Miss::NoPage {
        scheme: "dal".to_owned(),
        uri: u("confg"),
        nearest: Some(u("config")),
    };
    assert_eq!(
        miss_lines(&miss, "dalgon"),
        vec![
            format!("dalgon: no dal document at {}", u("confg")),
            format!(
                "Did you mean {}? Run dalgon docs for the list.",
                u("config")
            ),
        ]
    );
    let miss = Miss::NotAUri {
        text: "config".to_owned(),
        nearest: Some(u("config")),
    };
    assert_eq!(
        miss_lines(&miss, "dalgon")[0],
        "dalgon: \"config\" is not a document URI"
    );
}

#[test]
fn miss_write_error_is_reported() {
    // A closed pipe surfaces as a write failure, never a panic: the index
    // bytes are finite and writing them to an always-failing sink returns its
    // error. The process exit code follows the CLI part's pipe rule (open).
    use std::io::Write as _;

    struct Failing;
    impl std::io::Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }
    let snap = snapshot();
    let text = listing(&snap);
    let mut sink = Failing;
    assert!(sink.write_all(text.as_bytes()).is_err());
    assert_ne!(text, "");
}

#[test]
fn truth_dalgona_config_diff() {
    // Every `dalgona default` cell equals the Dalgona product default: symbol
    // search on, `hashline` edit style, guard on (the Dalgona product contract).
    let text = std::fs::read_to_string(repo_root().join("dalgona/docs/config.md")).expect("config");
    for (key, default) in [
        ("search_symbols", "true"),
        ("edit_style", "hashline"),
        ("[guard].enabled", "true"),
    ] {
        let row: Vec<_> = text
            .lines()
            .filter(|line| line.contains(&format!("`{key}`")))
            .collect();
        assert_eq!(row.len(), 1, "one row for {key}");
        assert!(
            row[0].contains(&format!("`{default}`")),
            "dalgona default for {key}: {}",
            row[0]
        );
    }
}

fn fenced(text: &str, tag: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") && trimmed[3..].trim() == tag {
            let mut block = String::new();
            for line in lines.by_ref() {
                if line.trim_start().starts_with("```") {
                    break;
                }
                block.push_str(line);
                block.push('\n');
            }
            blocks.push(block);
        }
    }
    blocks
}

fn extract_links(text: &str, scheme: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find("://") {
        let separator = cursor + relative;
        let start = separator.saturating_sub(scheme.len());
        if text.as_bytes().get(start..separator) == Some(scheme.as_bytes()) {
            let rest = &text[separator + 3..];
            let end = rest
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            '`' | '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
                        )
                })
                .unwrap_or(rest.len());
            let mut page = rest[..end].to_owned();
            while matches!(
                page.as_bytes().last(),
                Some(b'.' | b',' | b';' | b'!' | b'?' | b':')
            ) {
                page.pop();
            }
            if !page.is_empty() && page != "index" && !page.ends_with('/') {
                found.push(page);
            }
        }
        cursor = separator + 3;
    }
    found
}
