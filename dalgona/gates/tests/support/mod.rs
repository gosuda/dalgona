// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#![expect(
    clippy::disallowed_methods,
    reason = "gate support drives real binaries, scripts, and toolchain commands"
)]
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
pub(crate) type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) struct Scratch {
    path: PathBuf,
}

impl Scratch {
    pub(crate) fn new(label: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) fn run_command(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Output> {
    command.output()
}

pub(crate) const BATTERIES: [&str; 11] = [
    "ask",
    "history",
    "judged",
    "mcp",
    "orchestration",
    "quality",
    "review",
    "skills",
    "ttsr-rules",
    "web",
    "work",
];

pub(crate) fn build_product(
    root: PathBuf,
    user_toml: Option<&str>,
) -> TestResult<dal_agent::Product> {
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        user_toml,
    )?;
    let cx = dalgon::BuildCx {
        data_root: root,
        config: &config,
    };
    Ok(dalgona::build(&cx)?)
}

pub(crate) fn battery_names(product: &dal_agent::Product) -> std::collections::BTreeSet<&str> {
    product
        .extensions
        .iter()
        .map(dal_agent::ext::Extension::name)
        .filter(|name| BATTERIES.contains(name))
        .collect()
}

/// Finds the prebuilt `dalgona` binary next to the test executable's profile
/// directory, or under `CARGO_TARGET_DIR` when `build.build-dir` splits
/// intermediate artifacts from final binaries.
pub(crate) fn dalgona_binary() -> TestResult<PathBuf> {
    let file = format!("dalgona{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe()?;
    let profile = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("the test binary has no target directory")?;
    let mut candidates = vec![profile.join(&file)];
    if let (Some(target), Some(name)) = (std::env::var_os("CARGO_TARGET_DIR"), profile.file_name())
    {
        candidates.push(Path::new(&target).join(name).join(&file));
    }
    if let Some(found) = candidates.iter().find(|path| path.is_file()) {
        return Ok(found.clone());
    }
    Err(format!(
        "the Dalgona binary is missing at {}; run `cargo build -p dalgona --bin dalgona` before this gate",
        candidates[0].display()
    )
    .into())
}

pub(crate) async fn start_dalgona(root: PathBuf) -> TestResult<dal_agent::Host> {
    start_dalgona_with_config(root, None).await
}

pub(crate) async fn start_dalgona_with_config(
    root: PathBuf,
    user_toml: Option<&str>,
) -> TestResult<dal_agent::Host> {
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        user_toml,
    )?;
    let cx = dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    };
    let product = (factory.build)(&cx)?;
    let env = dal_agent::Env {
        vars: BTreeMap::new(),
        cwd: root.clone(),
        sandbox_helper: None,
    };
    Ok(dal_agent::Host::start(product, config, env).await?)
}

/// One scripted provider step that streams `text` and ends the turn.
pub(crate) fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":{}}},{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"end_turn"}}]}}"#,
        json_string(text)
    )
}

/// One scripted provider step that calls a single tool with `args_json`.
pub(crate) fn tool_step(id: &str, name: &str, args_json: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":{id},"name":{name}}},{{"type":"tool_calls_done","calls":[{{"id":{id},"name":{name},"args":{{"kind":"parsed","value":{args_json}}}}}]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"tool_use"}}]}}"#,
        id = json_string(id),
        name = json_string(name),
    )
}

const USAGE: &str = r#"{"input_tokens":12,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}"#;

fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            control if control.is_control() => {
                let _ = write!(quoted, "\\u{:04x}", u32::from(control));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Runs `dalgona -p` against a scripted provider in a temporary data root and
/// returns the session journal, one record per line.
///
/// `top_level_toml` is appended to `dal.toml` before the provider table, so it
/// can hold top-level keys such as `disabled_batteries`.
pub(crate) fn run_scripted_print(
    scratch: &Scratch,
    top_level_toml: &str,
    steps: &[String],
    prompt: &str,
) -> TestResult<String> {
    let config_home = scratch.path().join("xdg-config");
    let data_home = scratch.path().join("xdg-data");
    std::fs::create_dir_all(config_home.join("dalgona"))?;
    std::fs::create_dir_all(&data_home)?;
    let fixture = scratch.path().join("scripted.jsonl");
    std::fs::write(&fixture, steps.join("\n") + "\n")?;
    std::fs::write(
        config_home.join("dalgona/dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\n{top_level_toml}\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    let output = run_command(
        std::process::Command::new(dalgona_binary()?)
            .args(["-p", "--json", "--approval", "all", prompt])
            .current_dir(scratch.path())
            .env_clear()
            .env("HOME", scratch.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home),
    )?;
    if !output.status.success() {
        return Err(format!(
            "dalgona -p failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let mut journals = Vec::new();
    collect_journals(&data_home.join("dalgona/sessions"), &mut journals)?;
    match journals.as_slice() {
        [journal] => Ok(std::fs::read_to_string(journal)?),
        other => Err(format!("expected one session journal, found {}", other.len()).into()),
    }
}

fn collect_journals(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_journals(&path, found)?;
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
    Ok(())
}

/// The journal records of one kind, in order.
pub(crate) fn journal_records<'a>(journal: &'a str, kind: &str) -> Vec<&'a str> {
    let marker = format!(r#""type":"{kind}""#);
    journal
        .lines()
        .filter(|line| line.contains(&marker))
        .collect()
}
