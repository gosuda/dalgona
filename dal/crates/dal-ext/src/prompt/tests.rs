use super::*;

fn input<'a>(
    tool_lines: &'a [Box<str>],
    plugin_sections: &'a [(Box<str>, Box<str>)],
    environment: &'a [(Box<str>, Box<str>)],
    letter_fallback: &'a [Box<str>],
) -> PromptInput<'a> {
    PromptInput {
        product_name: "dal",
        product_version: "0.1",
        tool_lines,
        skills: Some("skill body\nkept as supplied"),
        rules: Some("rule body"),
        rulebook: Some("rulebook body"),
        plugin_sections,
        environment,
        instructions: Some("# Instructions from /project/AGENTS.md\nDo this"),
        system_md: Some("SYSTEM content"),
        letter_fallback,
        docs_line: None,
    }
}

#[test]
fn composition_uses_order_and_sorted_plugin_names() {
    let plugins = [
        (Box::from("zeta"), Box::from("zeta body")),
        (Box::from("alpha"), Box::from("alpha body")),
    ];
    let environment = [(Box::from("cwd"), Box::from("/work"))];
    let tools = [Box::from("read <path>"), Box::from("search <query>")];
    let fallbacks = [Box::from("fallback description")];
    let prompt = build(&input(&tools, &plugins, &environment, &fallbacks));

    let order = [
        "You are dal 0.1",
        D2_PREFERENCE_LINE,
        "- read <path>",
        "<skills>",
        "<rules>",
        "<rulebook>",
        "plugin 'alpha':",
        "plugin 'zeta':",
        "<environment>",
        "<instructions>",
        "<letter-fallback>",
        DEFAULT_DOCS_LINE,
    ];
    let mut previous = 0;
    for marker in order {
        let next = prompt[previous..]
            .find(marker)
            .expect("expected composition marker")
            + previous;
        assert!(next >= previous);
        previous = next + marker.len();
    }
    assert!(prompt.contains("skill body\nkept as supplied"));
    assert!(prompt.contains("<instructions>\n# Instructions from /project/AGENTS.md\nDo this\n\nSYSTEM content\n</instructions>"));
}

#[test]
fn rendered_instructions_keep_one_separator_before_system_text() {
    let files = [instructions::InstructionFile {
        path: "/project/AGENTS.md".into(),
        content: "Keep supplied content\n".into(),
        truncated: false,
    }];
    let rendered = instructions::render(&files).unwrap();
    let mut sections = input(&[], &[], &[], &[]);
    sections.instructions = Some(&rendered);
    let prompt = build(&sections);
    assert!(prompt.contains(
        "<instructions>\n# Instructions from /project/AGENTS.md\nKeep supplied content\n\nSYSTEM content\n</instructions>"
    ));
    for (body, system, joined) in [
        ("Keep\n", "SYSTEM content", "Keep\n\nSYSTEM content"),
        ("Keep\n\n\n", "SYSTEM content", "Keep\n\n\nSYSTEM content"),
        ("Keep", "\nSYSTEM content", "Keep\n\nSYSTEM content"),
        ("Keep\n", "\nSYSTEM content", "Keep\n\nSYSTEM content"),
    ] {
        sections.instructions = Some(body);
        sections.system_md = Some(system);
        assert!(build(&sections).contains(joined));
    }
}

#[test]
fn prefix_ignores_suffix_inputs_and_matches_emitted_prefix() {
    let plugins = [(Box::from("letters"), Box::from("plugin body"))];
    let first_environment = [(Box::from("cwd"), Box::from("/one"))];
    let second_environment = [(Box::from("cwd"), Box::from("/two"))];
    let tools = [Box::from("read <path>"), Box::from("search <query>")];
    let first_fallbacks = [Box::from("fallback description")];
    let second_fallbacks = [Box::from("different fallback")];
    let first = input(&tools, &plugins, &first_environment, &first_fallbacks);
    let mut second = input(&tools, &plugins, &second_environment, &second_fallbacks);
    second.instructions = Some("changed instructions");
    second.system_md = Some("changed system text");
    second.docs_line = Some("changed docs line");

    let first_prompt = build(&first);
    let second_prompt = build(&second);
    let prefix_len = prefix_bytes(&first);
    assert_eq!(prefix_len, prefix_bytes(&second));
    assert_eq!(
        &first_prompt.as_bytes()[..prefix_len],
        &second_prompt.as_bytes()[..prefix_len]
    );
    assert!(first_prompt[..prefix_len].ends_with("</plugins>\n"));
    assert_ne!(first_prompt, second_prompt);
}

#[test]
fn empty_sections_are_omitted_and_docs_can_be_replaced() {
    let no_plugins: [(Box<str>, Box<str>); 0] = [];
    let no_environment: [(Box<str>, Box<str>); 0] = [];
    let no_tools: [Box<str>; 0] = [];
    let empty_fallback = [Box::from("")];
    let mut empty = input(&no_tools, &no_plugins, &no_environment, &empty_fallback);
    empty.skills = Some("");
    empty.rules = None;
    empty.rulebook = Some("");
    empty.instructions = Some("");
    empty.system_md = None;
    empty.docs_line = Some("custom docs line");

    let prompt = build(&empty);
    for omitted in [
        "<tools>",
        "<skills>",
        "<rules>",
        "<rulebook>",
        "<plugins>",
        "<environment>",
        "<instructions>",
        "<letter-fallback>",
        DEFAULT_DOCS_LINE,
    ] {
        assert!(!prompt.contains(omitted));
    }
    assert!(prompt.ends_with("custom docs line\n"));
}

#[test]
fn extension_builds_identity_section() {
    let built = extension().expect("prompt extension builds");
    let section = built.prompt_section().expect("prompt section present");
    assert_eq!(section.order(), dal_agent::ext::PromptOrder::Identity);
    assert!(built.schemes().is_empty());
}
