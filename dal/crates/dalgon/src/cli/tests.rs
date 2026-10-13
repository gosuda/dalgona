//! Command-tree, validation, and completion tests.

use clap::{Parser, error::ErrorKind};

use super::{Cli, Commands, ModeArg, RuleTestSourceArg};

#[test]
fn json_and_print_flags_preserve_prompt_arguments() {
    let cli = Cli::try_parse_from(["dalgon", "--json", "-p", "review this"]).unwrap();
    assert!(cli.json && cli.print);
    assert_eq!(cli.prompts, ["review this"]);
}

#[test]
fn mode_values_and_rule_source_are_closed_enums() {
    let cli = Cli::try_parse_from(["dalgon", "--mode", "eval-first"]).unwrap();
    assert_eq!(cli.mode, Some(ModeArg::EvalFirst));
    let rules = Cli::try_parse_from(["dalgon", "rules", "test", "input"]).unwrap();
    let Some(Commands::Rules(args)) = rules.command else {
        panic!("rules subcommand was not parsed");
    };
    let Some(super::RulesSubcommand::Test(args)) = args.command else {
        panic!("rules test subcommand was not parsed");
    };
    assert_eq!(args.source, RuleTestSourceArg::Text);
}

#[test]
fn invalid_approval_and_shell_values_are_usage_errors() {
    let approval = Cli::try_parse_from(["dalgon", "--approval", "allow"]).unwrap_err();
    assert_eq!(approval.kind(), ErrorKind::InvalidValue);
    let shell = Cli::try_parse_from(["dalgon", "completion", "unknown"]).unwrap_err();
    assert_eq!(shell.kind(), ErrorKind::InvalidValue);
}

#[test]
fn continuation_flags_remain_available_for_edge_validation() {
    let cli = Cli::try_parse_from(["dalgon", "--continue", "--resume"]).unwrap();
    assert!(cli.continue_session);
    assert_eq!(cli.resume, Some(None));
}

#[test]
fn public_command_hides_only_the_sandbox_helper() {
    let command = super::command();
    let names: Vec<_> = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .map(clap::Command::get_name)
        .collect();
    assert!(names.contains(&"app-server"));
    assert!(names.contains(&"completion"));
    assert!(!names.contains(&"__sandbox"));
}

#[test]
fn version_errors_use_the_selected_binary_name_and_package_version() {
    let package = env!("CARGO_PKG_VERSION");
    let dal_error = super::command_for("dalgon")
        .try_get_matches_from(["dalgon", "--version"])
        .unwrap_err();
    assert_eq!(dal_error.kind(), ErrorKind::DisplayVersion);
    assert_eq!(
        dal_error.to_string().trim_end(),
        format!("dalgon {package}")
    );

    let dalgona_error = super::command_for("dalgona")
        .try_get_matches_from(["dalgona", "--version"])
        .unwrap_err();
    assert_eq!(
        dalgona_error.to_string().trim_end(),
        format!("dalgona {package}")
    );
}

#[test]
fn connect_token_file_is_explicit_and_requires_connect() {
    let no_token = Cli::try_parse_from(["dalgon", "--connect", "ws://remote.example/v1/ws"])
        .expect("connect without explicit token");
    assert_eq!(no_token.connect_token_file, None);

    let with_token = Cli::try_parse_from([
        "dalgon",
        "--connect",
        "ws://remote.example/v1/ws",
        "--token-file",
        "/tmp/dalgon.token",
    ])
    .expect("connect with explicit token");
    assert_eq!(
        with_token.connect_token_file,
        Some(std::path::PathBuf::from("/tmp/dalgon.token"))
    );

    let serve = Cli::try_parse_from(["dalgon", "serve", "--token-file", "/tmp/serve.token"])
        .expect("serve retains its own token-file argument");
    assert_eq!(serve.connect_token_file, None);
    let Some(Commands::Serve(args)) = serve.command else {
        panic!("serve subcommand was not parsed");
    };
    assert_eq!(
        args.token_file,
        Some(std::path::PathBuf::from("/tmp/serve.token"))
    );

    let error = Cli::try_parse_from(["dalgon", "--token-file", "/tmp/dalgon.token"])
        .expect_err("token file requires --connect");
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    let error = Cli::try_parse_from(["dalgon", "--token-file", "/tmp/dalgon.token", "serve"])
        .expect_err("root token file cannot leak into serve");
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn root_conflicts_reject_before_path_work() {
    let cli = Cli::try_parse_from(["dalgon", "--continue", "--resume"]).unwrap();
    assert_eq!(
        super::validate_root(&cli),
        Err(super::RootValidation::ContinueWithResume)
    );
    let cli =
        Cli::try_parse_from(["dalgon", "--mode", "normal", "-m", "dalgon/eval-first"]).unwrap();
    assert_eq!(
        super::validate_root(&cli),
        Err(super::RootValidation::ModeWithModeId)
    );
    let cli = Cli::try_parse_from(["dalgon", "--name", ""]).unwrap();
    assert_eq!(
        super::validate_root(&cli),
        Err(super::RootValidation::EmptyName)
    );
    let cli = Cli::try_parse_from(["dalgon", "--print", "--connect", "127.0.0.1:7437"]).unwrap();
    assert_eq!(
        super::validate_root(&cli),
        Err(super::RootValidation::ConnectHeadless)
    );
    assert!(super::is_mode_id("dalgon/normal"));
    assert!(!super::is_mode_id("anthropic/claude"));
}

#[test]
fn completion_scripts_contain_only_generated_text() {
    for shell in [
        super::ShellArg::Bash,
        super::ShellArg::Elvish,
        super::ShellArg::Fish,
        super::ShellArg::Powershell,
        super::ShellArg::Zsh,
    ] {
        let script = super::completion_script(shell, "dalgon");
        assert!(script.contains("dalgon"), "shell script names the binary");
        assert!(
            script.contains("app-server") || script.contains("completion"),
            "shell script completes visible subcommands"
        );
    }
}

#[test]
fn plugin_texts_match_plan_fixed_strings() {
    use super::texts;
    assert_eq!(
        texts::plugin_not_configured("focus"),
        "dalgon: plugin \"focus\" is not configured\nAdd \"focus\" to the plugins key in config.toml, then run dalgon plugin grant focus."
    );
    assert_eq!(
        texts::plugin_declares_no_services("focus"),
        "Plugin \"focus\" declares no services; no grant is needed."
    );
    assert_eq!(
        texts::plugin_granted("focus", "user", "fs.read, net"),
        "Granted \"focus\" (user): fs.read, net."
    );
    assert_eq!(
        texts::plugin_already_granted("focus", "bundled", "net"),
        "Already granted \"focus\" (bundled): net."
    );
    assert_eq!(
        texts::plugin_no_grants("focus"),
        "No persistent grants for \"focus\"."
    );
    assert_eq!(
        texts::plugin_revoked("focus", 2),
        "Revoked \"focus\": removed 2 persistent grant(s). Session grants remain until their sessions end."
    );
    assert_eq!(
        texts::plugin_list_row("focus", "user", "fs.read, net", "granted"),
        "focus (user): services: fs.read, net; persistent grant: granted"
    );
    assert_eq!(texts::PLUGIN_NONE, "No plugins configured.");
}
