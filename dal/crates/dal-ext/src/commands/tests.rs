//! Behavior tests for the built-in commands part.
//!
//! Renderer goldens live beside the renderer in `session.rs`; everything
//! here runs in-process over pure helpers. Handler paths that need a
//! host-minted [`CommandCx`](dal_agent::ext::command::CommandCx) (submit,
//! jobs, catalog-backed lines, session lookup) are covered by the loop and
//! plugin gates, not here.

use dal_core::command::{Chooser, ErrorTriple, FrontAction, Output, Reply};

use super::misc::{nothing_to_compact, share, trust_text};
use super::model::{
    ambiguous_model, no_catalog, parse_mode, parse_thinking, resolve_model, unknown_mode,
    unknown_model, unknown_provider, unknown_thinking, unsupported_thinking,
};
use super::session::{
    check_name, copy_limit_check, export_format, no_reply, no_session, nothing_to_export,
};
use super::tree::{empty_session, no_fork_message, nothing_to_clone};
use super::{
    Arity, BUILTINS, MODE, RejectKind, Rejected, check_arity, plugin_raised, render, render_import,
    spec, startup_error, suggest_in, valid_name,
};

fn triple(what: &str, why: &str, fix: &str) -> ErrorTriple {
    ErrorTriple {
        what: what.into(),
        why: why.into(),
        fix: fix.into(),
    }
}

fn lex_error(raw: &str) -> dal_core::command::LexError {
    dal_core::command::tokens(raw).expect_err("lexer accepted an invalid tail")
}
#[test]
fn registry_order() {
    let names: Vec<&str> = BUILTINS.iter().map(|record| record.name).collect();
    assert_eq!(
        names,
        [
            "settings",
            "model",
            "tree",
            "thinking",
            "scoped-models",
            "export",
            "import",
            "share",
            "bug",
            "copy",
            "name",
            "session",
            "changelog",
            "hotkeys",
            "fork",
            "clone",
            "trust",
            "login",
            "logout",
            "new",
            "compact",
            "resume",
            "reload",
            "quit",
        ]
    );
    assert!(spec("mode").is_some());
    assert_eq!(spec("mode"), Some(&MODE));
    assert_eq!(spec("docs"), None);
    assert_eq!(
        BUILTINS[4],
        super::BuiltinSpec {
            name: "scoped-models",
            hint: "",
            summary: "choose the models that /model lists",
            scope: super::Scope::Session,
            idle: false,
            arity: super::Arity::None,
            single_instance: false,
        }
    );
    assert_eq!(BUILTINS[5].single_instance, true);
    assert_eq!(BUILTINS[15].single_instance, true);
    assert_eq!(BUILTINS[20].single_instance, true);
    assert_eq!(BUILTINS[22].single_instance, true);
    assert_eq!(BUILTINS[14].single_instance, false);
}

#[test]
fn startup_pairs() {
    let rejected = Rejected {
        plugin: "deploy",
        plugin_star: "/data/plugins/deploy/plugin.star",
        name: "model",
        other_plugin: None,
    };
    assert_eq!(
        startup_error(RejectKind::BuiltinClash, &rejected),
        (
            "dalgon: plugin \"deploy\" registers /model: that name belongs to a built-in command"
                .to_owned(),
            "Rename the command in /data/plugins/deploy/plugin.star, or remove \"deploy\" from the plugins key."
                .to_owned(),
        )
    );
    let two = Rejected {
        plugin: "a",
        plugin_star: "/a/plugin.star",
        name: "deploy",
        other_plugin: Some("b"),
    };
    assert_eq!(
        startup_error(RejectKind::TwoPlugins, &two),
        (
            "dalgon: plugins \"a\" and \"b\" both register /deploy".to_owned(),
            "Rename the command in one of them, or remove one of them from the plugins key."
                .to_owned(),
        )
    );
    assert_eq!(
        startup_error(RejectKind::InvalidName, &rejected),
        (
            "dalgon: plugin \"deploy\" registers an invalid command name \"model\"".to_owned(),
            "Use lowercase letters, digits, and hyphens, start with a letter, and use at most 32 characters."
                .to_owned(),
        )
    );
    assert_eq!(
        startup_error(RejectKind::TwiceInOnePlugin, &rejected),
        (
            "dalgon: plugin \"deploy\" registers /model twice".to_owned(),
            "Remove one of the two registrations in /data/plugins/deploy/plugin.star.".to_owned(),
        )
    );
    assert_eq!(
        startup_error(RejectKind::SummaryOrHintRange, &rejected),
        (
            "dalgon: plugin \"deploy\" gives /model an invalid summary".to_owned(),
            "Use one line of 1 to 80 bytes, and a hint of at most 40 bytes.".to_owned(),
        )
    );
    assert!(valid_name("deploy"));
    assert!(valid_name("skill:ask"));
    assert!(!valid_name("Deploy"));
    assert!(!valid_name("skill:"));
}

#[test]
fn busy_forms() {
    use dal_core::command::{BusyState, CommandError};

    assert_eq!(
        render(&CommandError::Busy {
            cmd: "fork".into(),
            state: BusyState::Running,
        }),
        triple(
            "A turn is running",
            "/fork needs an idle session",
            "Press esc to stop the turn, or wait for it to end.",
        )
    );
    assert_eq!(
        render(&CommandError::Busy {
            cmd: "fork".into(),
            state: BusyState::Compacting,
        }),
        triple(
            "The session is compacting",
            "/fork needs an idle session",
            "Wait for the compaction to end.",
        )
    );
    assert_eq!(
        render(&CommandError::Arity {
            cmd: "copy".into(),
            args: "1".into(),
        }),
        triple(
            "/copy takes no arguments",
            "the text after it was \"1\"",
            "Type /copy alone.",
        )
    );
    assert_eq!(
        render(&CommandError::JobCap {
            cmd: "compact".into()
        }),
        triple(
            "/compact is already running",
            "this session runs one compact at a time",
            "Wait for it to end.",
        )
    );
}

#[test]
fn unknown_forms() {
    use dal_core::command::CommandError;

    let names: Vec<&str> = BUILTINS
        .iter()
        .map(|record| record.name)
        .chain(std::iter::once(MODE.name))
        .collect();
    assert_eq!(suggest_in(&names, "modle"), Some("model"));
    assert_eq!(suggest_in(&names, "xyzzy"), None);
    assert_eq!(
        render(&CommandError::Unknown {
            name: "modle".into(),
            suggestion: Some("model".into()),
        }),
        triple(
            "Unknown command /modle",
            "no built-in or plugin command has this name",
            "Did you mean /model? Start the message with a space to send it as text.",
        )
    );
    assert_eq!(
        render(&CommandError::Unknown {
            name: "xyzzy".into(),
            suggestion: None,
        }),
        triple(
            "Unknown command /xyzzy",
            "no built-in or plugin command has this name",
            "Type / to see the commands, or start the message with a space to send it as text.",
        )
    );
    assert_eq!(
        render(&CommandError::Lex {
            args: "\"abc".into(),
            error: lex_error("\"abc"),
        }),
        triple(
            "The arguments have an unclosed double quote",
            "\"abc",
            "Close the quote, or remove it.",
        )
    );
    assert_eq!(
        render(&CommandError::Lex {
            args: "ab\\".into(),
            error: lex_error("ab\\"),
        }),
        triple(
            "The arguments end with a backslash",
            "ab\\",
            "Remove the backslash, or add the character it escapes.",
        )
    );
    assert_eq!(
        render(&CommandError::PluginCap),
        triple(
            "Too many plugin commands are running",
            "this session runs at most 16",
            "Wait for one to end.",
        )
    );
    assert_eq!(
        plugin_raised("deploy", "ship", "boom"),
        triple(
            "/deploy failed",
            "the plugin \"ship\" raised boom",
            "Report this to the plugin author.",
        )
    );
}

#[test]
fn import_pairs() {
    use dal_core::command::ImportFailure;

    assert_eq!(
        render_import(&ImportFailure::MissingFile {
            abs: "/tmp/out.jsonl".into(),
        }),
        triple(
            "Cannot import /tmp/out.jsonl",
            "the file does not exist",
            "Check the path, or write a session with /export PATH.jsonl.",
        )
    );
    assert_eq!(
        render_import(&ImportFailure::LineTooLong {
            abs: "/tmp/out.jsonl".into(),
            line: 41,
        }),
        triple(
            "Cannot import /tmp/out.jsonl",
            "line 41 is longer than 16 MiB",
            "Check the path, or write a session with /export PATH.jsonl.",
        )
    );
}

#[test]
fn arity_contract() {
    use dal_core::command::CommandError;
    assert_eq!(check_arity("copy", Arity::None, "").unwrap(), None);
    assert_eq!(check_arity("copy", Arity::None, "   ").unwrap(), None);
    assert!(check_arity("copy", Arity::None, "1").is_err());
    assert_eq!(check_arity("model", Arity::Optional, "").unwrap(), None);
    assert_eq!(
        check_arity("model", Arity::Optional, "openai/gpt").unwrap(),
        Some("openai/gpt".into())
    );
    assert!(check_arity("model", Arity::Optional, "a b").is_err());
    assert!(check_arity("model", Arity::Optional, "--limit=5 --limit=5").is_err());
    assert_eq!(
        check_arity("model", Arity::Optional, "'openai gpt'").unwrap(),
        Some("openai gpt".into())
    );
    assert_eq!(
        check_arity("model", Arity::Optional, r"openai\ gpt").unwrap(),
        Some("openai gpt".into())
    );
    assert!(check_arity("model", Arity::Optional, "\"a\" 'b c'").is_err());
    assert!(check_arity("model", Arity::Optional, "$(a) $(b)").is_err());
    assert!(matches!(
        check_arity("copy", Arity::None, "\"abc"),
        Err(CommandError::Lex { .. })
    ));
    assert!(matches!(
        check_arity("copy", Arity::None, "'abc"),
        Err(CommandError::Lex { .. })
    ));
    assert!(matches!(
        check_arity("copy", Arity::None, "abc\\"),
        Err(CommandError::Lex { .. })
    ));
}

#[test]
fn model_resolution_matrix() {
    use dal_core::{Family, Mode, ModelRoute};

    use super::model::ModelResolution;

    let pairs = [
        ("openai", "gpt-x"),
        ("anthropic", "claude"),
        ("other", "gpt-x"),
    ];
    match resolve_model(&pairs, "openai/gpt-x") {
        ModelResolution::Route {
            provider,
            id,
            route,
        } => {
            assert_eq!(&*provider, "openai");
            assert_eq!(&*id, "gpt-x");
            assert_eq!(
                route,
                ModelRoute::Api {
                    family: Family::Responses,
                    model: "gpt-x".into(),
                }
            );
        }
        other => panic!("qualified row must resolve, got {other:?}"),
    }
    match resolve_model(&pairs, "claude") {
        ModelResolution::Route { provider, id, .. } => {
            assert_eq!(&*provider, "anthropic");
            assert_eq!(&*id, "claude");
        }
        other => panic!("bare unique id must resolve, got {other:?}"),
    }
    match resolve_model(&pairs, "gpt-x") {
        ModelResolution::Ambiguous { first, second } => {
            assert_eq!(&*first, "openai");
            assert_eq!(&*second, "other");
        }
        other => panic!("shared bare id must be ambiguous, got {other:?}"),
    }
    assert!(matches!(
        resolve_model(&pairs, "dalgon/eval-first"),
        ModelResolution::Mode(Mode::EvalFirst)
    ));
    assert!(matches!(
        resolve_model(&pairs, "zzz"),
        ModelResolution::Unknown
    ));
    assert!(matches!(
        resolve_model(&pairs, "dalgon/nope"),
        ModelResolution::Unknown
    ));
    assert!(matches!(
        resolve_model(&pairs, "openai/nope"),
        ModelResolution::Unknown
    ));
    assert_eq!(
        unknown_model("zzz"),
        triple(
            "Unknown model \"zzz\"",
            "it is not a cached provider id, an alias, or a dalgon mode",
            "Type /model to pick from the list.",
        )
    );
    assert_eq!(
        ambiguous_model("gpt-x", "openai", "other"),
        triple(
            "Model id \"gpt-x\" is ambiguous",
            "it matches openai and other",
            "Type /model openai/gpt-x or /model other/gpt-x.",
        )
    );
    assert_eq!(parse_mode("normal"), Some(Mode::Normal));
    assert_eq!(parse_mode("eval-first"), Some(Mode::EvalFirst));
    assert_eq!(parse_mode("eval-only"), Some(Mode::EvalOnly));
    assert_eq!(parse_mode("loud"), None);
    assert_eq!(
        unknown_mode("loud"),
        triple(
            "Unknown mode \"loud\"",
            "the modes are normal, eval-first, eval-only",
            "Type /mode normal, /mode eval-first, or /mode eval-only.",
        )
    );
    assert_eq!(
        unknown_mode(""),
        triple(
            "Unknown mode \"\"",
            "the modes are normal, eval-first, eval-only",
            "Type /mode normal, /mode eval-first, or /mode eval-only.",
        )
    );
}

#[test]
fn thinking_matrix() {
    use dal_core::ThinkingLevel;

    assert_eq!(parse_thinking("off"), Some(ThinkingLevel::Off));
    assert_eq!(parse_thinking("minimal"), Some(ThinkingLevel::Minimal));
    assert_eq!(parse_thinking("low"), Some(ThinkingLevel::Low));
    assert_eq!(parse_thinking("medium"), Some(ThinkingLevel::Medium));
    assert_eq!(parse_thinking("high"), Some(ThinkingLevel::High));
    assert_eq!(parse_thinking("xhigh"), Some(ThinkingLevel::Xhigh));
    assert_eq!(parse_thinking("max"), Some(ThinkingLevel::Max));
    assert_eq!(parse_thinking("loud"), None);
    assert_eq!(
        unknown_thinking("hgih"),
        triple(
            "Unknown thinking level \"hgih\"",
            "the levels are off, minimal, low, medium, high, xhigh, max",
            "Did you mean high? Type /thinking to pick from the list.",
        )
    );
    assert_eq!(
        unknown_thinking("loud"),
        triple(
            "Unknown thinking level \"loud\"",
            "the levels are off, minimal, low, medium, high, xhigh, max",
            "Type /thinking to pick from the list.",
        )
    );
    assert_eq!(
        unsupported_thinking("max", "openai/gpt-x", "off, low"),
        triple(
            "Thinking level \"max\" is not available",
            "openai/gpt-x supports off, low",
            "Type /thinking with one of those levels.",
        )
    );
    assert_eq!(
        no_catalog(),
        triple(
            "No model list is cached",
            "dalgon has not fetched one yet",
            "Type /model to fetch the list, then try again.",
        )
    );
}

#[test]
fn name_rules() {
    assert_eq!(check_name(&"a".repeat(64)).unwrap(), "a".repeat(64));
    assert_eq!(
        check_name(&"\u{1F600}".repeat(64)).unwrap(),
        format!("\"{}\"", "\u{1F600}".repeat(64))
    );
    assert_eq!(
        check_name(&"a".repeat(65)).unwrap_err(),
        triple(
            "The name is too long: it has 65 characters and the limit is 64",
            "names hold at most 64 characters",
            "Type a shorter name.",
        )
    );
    assert_eq!(
        check_name("a\tb").unwrap_err(),
        triple(
            "The name holds a control character",
            "names must be printable text",
            "Type the name again without tabs or line breaks.",
        )
    );
    assert_eq!(check_name("my session").unwrap(), "\"my session\"");
    assert_eq!(check_name("my-session_1.x").unwrap(), "my-session_1.x");
}

#[test]
fn copy_pairs() {
    assert_eq!(
        no_reply(),
        triple(
            "There is no reply to copy",
            "the session has no assistant message yet",
            "Send a message first.",
        )
    );
    assert!(copy_limit_check(0).is_ok());
    assert!(copy_limit_check(4_718_592).is_ok());
    assert_eq!(
        copy_limit_check(4_718_593).unwrap_err(),
        triple(
            "The last reply is too large to copy: it has 4718593 bytes and OSC 52 carries at most 6291456",
            "the clipboard path cannot carry it",
            "Type /export to write the session to a file.",
        )
    );
    assert_eq!(
        copy_limit_check(7 * 1024 * 1024).unwrap_err().what.as_ref(),
        "The last reply is too large to copy: it has 7340032 bytes and OSC 52 carries at most 6291456",
    );
}

#[test]
fn login_logout_flows() {
    use super::model::{login, logout};

    assert_eq!(
        login(None).unwrap(),
        Reply::Choose {
            chooser: Chooser::Login,
            filter: "".into(),
        }
    );
    assert_eq!(
        login(Some("anthropic")).unwrap(),
        Reply::Front(FrontAction::Login {
            provider: "anthropic".into(),
        })
    );
    assert_eq!(
        login(Some("openai-codexx")).unwrap_err(),
        triple(
            "Unknown provider \"openai-codexx\"",
            "dalgon signs in to anthropic, openai, and openai-codex",
            "Did you mean openai-codex? Type /login to pick from the list.",
        )
    );
    assert_eq!(
        unknown_provider("logout", "xyz"),
        triple(
            "Unknown provider \"xyz\"",
            "dalgon signs in to anthropic, openai, and openai-codex",
            "Type /logout to pick from the list.",
        )
    );
    assert_eq!(
        logout(Some("openai")).unwrap(),
        Reply::Front(FrontAction::Logout {
            provider: "openai".into(),
        })
    );
}

#[test]
fn refusals_and_front_data() {
    use super::misc::bug;
    use super::session::{hotkeys, new_session};

    assert_eq!(
        share().unwrap(),
        Reply::Done(Output::Text(
            "/share is not in dalgon: dalgon uploads no session data to any service\nType /export to write the session to a file, then share the file yourself."
                .into(),
        ))
    );
    assert_eq!(
        bug().unwrap(),
        Reply::Done(Output::Text(
            "/bug is not in dalgon: dalgon sends no reports or session data to its developers\nType /export to write the session to a file, and attach it with the log path from /session to your report."
                .into(),
        ))
    );
    assert_eq!(
        trust_text(std::path::Path::new("/data")),
        "/trust is not in dalgon: dalgon loads plugins only from its data directory and reads AGENTS.md in every workspace, so there is no project trust to save\nPut plugins under /data/plugins and name them in the plugins key of dal.toml.",
    );
    assert_eq!(hotkeys(), Reply::Front(FrontAction::ShowKeys));
    assert_eq!(new_session(), Reply::Front(FrontAction::NewSession));
}

#[test]
fn reload_counts() {
    use dal_agent::ext::command::ReloadSummary;

    use super::misc::reload_text;

    let text = |summary: ReloadSummary| reload_text(&summary);
    assert_eq!(
        text(ReloadSummary {
            plugins: 2,
            tools: 3
        }),
        "Reloaded 2 plugins: 3 tools. New turns use them. A running turn keeps the plugins it started with.",
    );
    assert_eq!(
        text(ReloadSummary {
            plugins: 1,
            tools: 1
        }),
        "Reloaded 1 plugin: 1 tool. New turns use them. A running turn keeps the plugins it started with.",
    );
    assert_eq!(
        text(ReloadSummary {
            plugins: 0,
            tools: 0
        }),
        "No plugins to reload: the plugins key in dal.toml is empty.",
    );
}

#[test]
fn reload_publish_failure() {
    use dal_agent::ext::command::BuildError;

    use super::misc::publish_failure;

    let error =
        BuildError::validation(dal_core::RegistrationError::InvalidName { name: "x".into() });
    assert_eq!(
        publish_failure(error),
        triple(
            "reload failed",
            "invalid name \"x\"; names must match [a-z][a-z0-9_-]{0,63}",
            "the previous plugin set stays live",
        )
    );
}

#[test]
fn tree_and_job_pairs() {
    assert_eq!(
        empty_session(),
        triple(
            "The session is empty",
            "there is nothing to move to",
            "Send a message first.",
        )
    );
    assert_eq!(
        no_fork_message(),
        triple(
            "There is no message to fork from",
            "the session has no user message yet",
            "Send a message first.",
        )
    );
    assert_eq!(
        nothing_to_clone(),
        triple(
            "There is nothing to clone",
            "the session has no messages",
            "Send a message first.",
        )
    );
    assert_eq!(
        nothing_to_compact(),
        triple(
            "There is nothing to compact yet",
            "the session has no finished turn",
            "Send a message first.",
        )
    );
    assert_eq!(
        nothing_to_export(),
        triple(
            "There is nothing to export",
            "the session has no messages",
            "Send a message first.",
        )
    );
    assert_eq!(
        no_session("moon"),
        triple(
            "No session named \"moon\" in this workspace",
            "dalgon looked for an id or a name",
            "Type /resume to pick from the list.",
        )
    );
}

#[test]
fn export_format_pairs() {
    use dal_core::command::ExportFormat;

    assert_eq!(export_format("md").unwrap(), ExportFormat::Markdown);
    assert_eq!(export_format("jsonl").unwrap(), ExportFormat::Jsonl);
    assert_eq!(
        export_format("txt").unwrap_err(),
        triple(
            "dalgon cannot export \"txt\" files",
            "export writes .md or .jsonl",
            "Type /export PATH.md or /export PATH.jsonl.",
        )
    );
}
