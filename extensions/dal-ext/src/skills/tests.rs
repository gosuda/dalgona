use super::invoke::{listing, reminder};
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
        let err = register("p", "s", description, "SKILL.md", BodyInput::Bytes(b"x")).unwrap_err();
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
        b"\xef\xbb\xbf---\r\nname: not-frontmatter\r\n---\r\n\t# Body \xea\xb0\x80  \n\n".to_vec();
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
    assert_eq!(conflict.registry.names(), []);
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
    assert_eq!(conflict.registry.names(), []);
}

#[test]
fn zero_skills_remove_section() {
    let registry = SkillRegistry::merge(&[("p", Vec::new())]).unwrap();
    assert_eq!(registry.section(&empty()), None);
    assert_eq!(registry.names(), []);
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

#[test]
fn front_matter_mcp_fills_the_record_and_bad_keys_name_the_file_position() {
    let good =
        b"---\nmcp:\n  servers:\n    web:\n      url: https://docs.example/mcp\n---\n# Body\n";
    let loaded = register("p", "s", "d", "SKILL.md", BodyInput::Bytes(good)).unwrap();
    let block = loaded.mcp.expect("the front matter declares one server");
    assert_eq!(
        block.servers["web"],
        dal_core::ext::McpServerDecl::Http {
            endpoint: dal_core::ext::McpEndpoint::parse("https://docs.example/mcp")
                .expect("fixture endpoint"),
        }
    );
    assert_eq!(&*loaded.body, std::str::from_utf8(good).unwrap());

    let bad =
        b"---\nmcp:\n  servers:\n    web:\n      url: https://h.example\n      port: 1\n---\n";
    let err = register("p", "s", "d", "SKILL.md", BodyInput::Bytes(bad)).unwrap_err();
    let diagnostic = err.to_string();
    assert!(
        diagnostic.starts_with(&format!("{}:6:7:", dir("p").join("SKILL.md").display())),
        "the invalid key must retain its file and position: {diagnostic}"
    );
    assert!(diagnostic.contains("unknown key \"port\""), "{diagnostic}");
}

#[test]
fn front_matter_launch_decodes_and_the_command_alias_stays_rejected() {
    let good = b"---\nmcp:\n  servers:\n    files:\n      launch:\n        kind: npx\n        package: files-server\n        args: [\"files server\"]\n---\n# Body\n";
    let loaded = register("p", "s", "d", "SKILL.md", BodyInput::Bytes(good)).unwrap();
    let block = loaded.mcp.expect("the front matter declares one server");
    assert_eq!(
        block.servers["files"],
        dal_core::ext::McpServerDecl::Stdio {
            launch: dal_core::ext::McpLaunch::npx(
                "files-server".into(),
                None,
                vec!["files server".into()],
            )
            .expect("fixture launch"),
            env: std::collections::BTreeMap::default(),
        }
    );

    let alias = b"---\nmcp:\n  servers:\n    files:\n      command: [npx, files-server]\n---\n";
    let err = register("p", "s", "d", "SKILL.md", BodyInput::Bytes(alias)).unwrap_err();
    let diagnostic = err.to_string();
    assert!(
        diagnostic.contains("\"command\" was renamed to \"launch\""),
        "the old command form must name its replacement: {diagnostic}"
    );
    assert!(
        diagnostic.starts_with(&format!("{}:5:7:", dir("p").join("SKILL.md").display())),
        "alias rejection retains its source position: {diagnostic}"
    );
}

fn search_registry() -> SkillRegistry {
    SkillRegistry::merge(&[
        (
            "p",
            vec![
                skill(
                    "p",
                    "find-anything",
                    "Pick the right search surface.",
                    false,
                ),
                skill("p", "find-files", "List files by name.", false),
            ],
        ),
        (
            "q",
            vec![
                skill("q", "review", "Review a diff.", false),
                skill(
                    "q",
                    "delegate-with-contracts",
                    "Hand work to a subagent.",
                    false,
                ),
            ],
        ),
    ])
    .unwrap()
}

fn names_of(skills: Vec<&RegisteredSkill>) -> Vec<&str> {
    skills.into_iter().map(|skill| &*skill.name).collect()
}

#[test]
fn prefix_lists_candidates_and_resolves_nothing() {
    let registry = search_registry();
    assert_eq!(
        listing(&registry, "find-"),
        "Skills matching \"find-\" (2):\n\
         - find-anything: Pick the right search surface.\n\
         - find-files: List files by name.\n\
         Nothing was run. Run one with /skill:<name> or $<name>."
    );
    for token in ["find-", "$find-", "/skill:find-", "$p:find-"] {
        assert!(
            registry.invoked(token).is_empty(),
            "{token} is a prefix and must resolve nothing"
        );
    }
    assert_eq!(
        names_of(registry.invoked("/skill find-")),
        Vec::<&str>::new()
    );
    assert_eq!(
        names_of(registry.invoked("$find-anything")),
        ["find-anything"],
        "an exact name still resolves"
    );
}

#[test]
fn candidates_contain_the_text_without_case_folding() {
    let registry = search_registry();
    assert_eq!(names_of(registry.candidates("-fi")), ["find-files"]);
    assert_eq!(
        names_of(registry.candidates("tracts")),
        ["delegate-with-contracts"]
    );
    assert_eq!(names_of(registry.candidates("FIND")), Vec::<&str>::new());
    assert_eq!(
        names_of(registry.candidates("find-")),
        ["find-anything", "find-files"]
    );
    assert_eq!(
        names_of(registry.candidates("")),
        [
            "delegate-with-contracts",
            "find-anything",
            "find-files",
            "review"
        ]
    );
}

#[test]
fn listing_states_each_empty_case() {
    let registry = search_registry();
    assert_eq!(
        listing(&registry, "zzz"),
        "No skill name contains \"zzz\". Nothing was run."
    );
    assert!(listing(&registry, "").starts_with("Skills (4):\n- delegate-with-contracts: "));
    assert_eq!(
        listing(&SkillRegistry::empty(), "find-"),
        "No skills are loaded."
    );
}

#[test]
fn listing_cuts_a_long_description_on_a_character_boundary() {
    let long = format!("First {}", "\u{ac00}".repeat(60));
    let registry = SkillRegistry::merge(&[("p", vec![skill("p", "long", &long, false)])]).unwrap();
    let text = listing(&registry, "long");
    let line = text.lines().nth(1).unwrap();
    assert!(line.starts_with("- long: First \u{ac00}"), "{line}");
    assert!(line.ends_with('…'), "{line}");
    assert!(line.len() <= "- long: ".len() + 120, "{line}");
}

#[test]
fn prompt_tokens_name_skills_once_in_first_mention_order() {
    let registry = search_registry();
    let named = registry
        .invoked("use $review then $find-anything and /skill:review again, plus $p:find-files");
    assert_eq!(names_of(named), ["review", "find-anything", "find-files"]);
}

#[test]
fn only_whole_registered_tokens_name_skills() {
    let registry = search_registry();
    for text in [
        "$q:find-files",
        "$Review",
        "$review,",
        "x$review",
        "$",
        "$:review",
        "$review:",
        "$HOME",
        "/skill:",
        "/skill:nope",
        "skill:review",
        "review",
    ] {
        assert!(
            registry.invoked(text).is_empty(),
            "{text:?} must stay plain text"
        );
    }
    assert_eq!(
        names_of(registry.invoked("a\t$review\n$q:review")),
        ["review"]
    );
}

#[test]
fn reminder_carries_each_body_whole() {
    let registry = search_registry();
    assert_eq!(reminder(&registry.invoked("no skill here")), None);
    let mut with_newline = skill("n", "ends-in-newline", "d", false);
    with_newline.body = Arc::from("line\n");
    let merged = SkillRegistry::merge(&[(
        "n",
        vec![with_newline, skill("n", "plain-body", "d", false)],
    )])
    .unwrap();
    assert_eq!(
        reminder(&merged.invoked("$plain-body $ends-in-newline")).as_deref(),
        Some(
            "<skill name=\"plain-body\">\nbody of plain-body\n</skill>\n\
             <skill name=\"ends-in-newline\">\nline\n</skill>\n"
        )
    );
}

#[test]
fn command_summary_is_cut_to_a_valid_short_line() {
    let description = "\u{ac00}".repeat(60);
    let registry =
        SkillRegistry::merge(&[("p", vec![skill("p", "wide", &description, false)])]).unwrap();
    let built = extension(Arc::new(registry)).unwrap();
    let summary = &built.commands()[1].0.summary;
    assert!(summary.len() <= 80 && summary.ends_with('…'), "{summary}");
    let exact = "a".repeat(80);
    let registry =
        SkillRegistry::merge(&[("p", vec![skill("p", "exact", &exact, false)])]).unwrap();
    let built = extension(Arc::new(registry)).unwrap();
    assert_eq!(&*built.commands()[1].0.summary, exact);
    let over = "a".repeat(81);
    let registry = SkillRegistry::merge(&[("p", vec![skill("p", "over", &over, false)])]).unwrap();
    let built = extension(Arc::new(registry)).unwrap();
    let summary = &built.commands()[1].0.summary;
    assert_eq!(summary.len(), 80, "{summary}");
    assert_eq!(&summary[..77], "a".repeat(77).as_str());
    assert!(summary.ends_with('…'), "{summary}");
    let split = format!("{}{}{}", "a".repeat(76), "\u{ac00}", "b".repeat(10));
    let registry =
        SkillRegistry::merge(&[("p", vec![skill("p", "split", &split, false)])]).unwrap();
    let built = extension(Arc::new(registry)).unwrap();
    let summary = &built.commands()[1].0.summary;
    assert!(summary.len() < 80, "{summary}");
    assert_eq!(&**summary, format!("{}…", "a".repeat(76)).as_str());
}

#[test]
fn skill_uri_reads() {
    // Registered across two plugins out of byte order: plugin-b's alpha
    // claims before plugin-a's beta, and the registry still reports both in
    // its byte-sorted name order.
    let alpha = skill("plugin-b", "alpha", "alpha description", false);
    let beta = skill("plugin-a", "beta", "beta description", false);
    let registry =
        SkillRegistry::merge(&[("plugin-b", vec![alpha]), ("plugin-a", vec![beta])]).unwrap();
    let names: Vec<String> = registry.names().iter().map(ToString::to_string).collect();
    let resolver = SkillResolver::new(Arc::new(registry));

    // A hit serves the exact loaded body under its canonical URI.
    for name in &names {
        let doc = resolver.lookup(name).unwrap();
        assert_eq!(&*doc.uri, format!("skill://{name}").as_str());
        assert_eq!(&*doc.text, format!("body of {name}").as_str());
    }

    // A miss is the typed refusal that names the missing skill. The
    // alternatives listing is checked once, with live product records,
    // in the skills-defaults gate.
    let err = resolver.lookup("gamma").unwrap_err();
    assert!(matches!(&err, SchemeError::Failed { .. }));
    let text = err.to_string();
    assert!(text.contains("gamma"), "{text}");

    // An empty registry refuses through the same typed error variant.
    let empty = SkillResolver::new(Arc::new(SkillRegistry::empty()));
    let empty_err = empty.lookup("gamma").unwrap_err();
    assert!(matches!(empty_err, SchemeError::Failed { .. }));
}
