//! Offline commands for inspecting and testing stream rules.

use std::{
    io::{self, Write},
    path::Path,
    process::ExitCode,
};

use dal_core::RulesConfig;
use dal_ext::ttsr::build::RuleBuildInput;
use dal_ext::ttsr::report::{self, TestFlags, TestSource, UsageError};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_TEST_INPUT_BYTES: usize = 1_048_576;

/// The stream channel used by an offline rule simulation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleTestSource {
    /// Simulate assistant prose.
    Text,
    /// Simulate assistant reasoning.
    Thinking,
    /// Simulate one tool call's added text.
    Tool,
}

/// Inputs for `dalgon rules test`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleTestArgs {
    /// The output channel to simulate.
    pub source: RuleTestSource,
    /// Tool name for a tool-source simulation; the CLI defaults this to `patch`.
    pub tool: String,
    /// Optional path context for a tool-source simulation.
    pub path: Option<String>,
    /// Text to test, or `-` to read at most 1 MiB from standard input.
    pub text: String,
}

/// An offline rules command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RulesCommand {
    /// List the effective rules and any skipped-rule problems.
    List,
    /// Simulate a rule set against one input.
    Test(RuleTestArgs),
}

/// A rules command could not produce its requested report.
#[derive(Debug, Error)]
pub enum RulesCommandError {
    /// The piped simulation text exceeded the 1 MiB limit.
    #[error(
        "dalgon: rules test input exceeds 1048576 bytes\nPass TEXT directly or pipe at most 1048576 bytes to dalgon rules test."
    )]
    InputTooLarge,
    /// The TTSR API rejected a rule-test option.
    #[error("{0}")]
    Usage(#[from] UsageError),
    /// Reading standard input or writing the report failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl RulesCommandError {
    /// Returns the process status for this failure.
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Usage(_) => ExitCode::from(124),
            Self::InputTooLarge | Self::Io(_) => ExitCode::FAILURE,
        }
    }
}

/// Runs a rules report or an offline rule simulation.
///
/// The rule engine owns discovery, parsing, precedence, and matching. This
/// module only adapts its report to the CLI and reads piped test input lazily.
///
/// # Errors
/// Returns a typed usage, standard-input, or standard-output failure.
pub async fn run(
    command: RulesCommand,
    input: &RuleBuildInput<'_>,
    data_root: &Path,
    workspace: &Path,
    config: &RulesConfig,
    stdin: &mut (impl AsyncRead + Unpin),
    stdout: &mut impl Write,
) -> Result<ExitCode, RulesCommandError> {
    match command {
        RulesCommand::List => {
            let result = report::run_rules(input, data_root, workspace, config);
            stdout.write_all(result.text.as_bytes())?;
            Ok(status(result.exit))
        }
        RulesCommand::Test(args) => {
            let text = if args.text == "-" {
                read_test_input(stdin).await?
            } else {
                args.text
            };
            let flags = TestFlags {
                source: match args.source {
                    RuleTestSource::Text => TestSource::Text,
                    RuleTestSource::Thinking => TestSource::Thinking,
                    RuleTestSource::Tool => TestSource::Tool,
                },
                tool: args.tool,
                path: args.path,
                text,
            };
            let result = report::run_test(input, data_root, workspace, config, &flags)?;
            stdout.write_all(result.text.as_bytes())?;
            Ok(status(result.exit))
        }
    }
}

fn status(code: i32) -> ExitCode {
    match code {
        0 => ExitCode::SUCCESS,
        2 => ExitCode::from(124),
        _ => ExitCode::FAILURE,
    }
}

async fn read_test_input(
    stdin: &mut (impl AsyncRead + Unpin),
) -> Result<String, RulesCommandError> {
    let mut bytes = Vec::with_capacity(MAX_TEST_INPUT_BYTES);
    stdin
        .take((MAX_TEST_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_TEST_INPUT_BYTES {
        return Err(RulesCommandError::InputTooLarge);
    }
    String::from_utf8(bytes)
        .map_err(|error| RulesCommandError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };

    use tokio::io::ReadBuf;

    struct NeverRead(Arc<AtomicUsize>);

    impl AsyncRead for NeverRead {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Poll::Ready(Err(io::Error::other("stdin was read")))
        }
    }

    struct BytesRead {
        bytes: Vec<u8>,
        offset: usize,
    }

    impl AsyncRead for BytesRead {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let remaining = &self.bytes[self.offset..];
            let count = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..count]);
            self.offset += count;
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn ordinary_text_does_not_poll_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let mut stdin = NeverRead(Arc::clone(&reads));
        let mut stdout = Vec::new();
        let input = RuleBuildInput {
            records: &[],
            plugin_rules: &[],
            known_tools: &[],
            agent: "test",
        };
        let result = run(
            RulesCommand::Test(RuleTestArgs {
                source: RuleTestSource::Text,
                tool: "patch".to_owned(),
                path: None,
                text: "nothing to match".to_owned(),
            }),
            &input,
            dir.path(),
            dir.path(),
            &RulesConfig::default(),
            &mut stdin,
            &mut stdout,
        )
        .await
        .unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        assert_eq!(result, ExitCode::FAILURE);
        assert_eq!(stdout, b"No rule fired. Checked 0 stream rules.\n");
    }

    #[tokio::test]
    async fn piped_input_accepts_the_limit_and_rejects_one_more_byte() {
        let dir = tempfile::tempdir().unwrap();
        let mut stdin = BytesRead {
            bytes: vec![b'x'; MAX_TEST_INPUT_BYTES],
            offset: 0,
        };
        let mut stdout = Vec::new();
        let input = RuleBuildInput {
            records: &[],
            plugin_rules: &[],
            known_tools: &[],
            agent: "test",
        };
        let result = run(
            RulesCommand::Test(RuleTestArgs {
                source: RuleTestSource::Text,
                tool: "patch".to_owned(),
                path: None,
                text: "-".to_owned(),
            }),
            &input,
            dir.path(),
            dir.path(),
            &RulesConfig::default(),
            &mut stdin,
            &mut stdout,
        )
        .await
        .unwrap();
        assert_eq!(result, ExitCode::FAILURE);
        assert_eq!(stdout, b"No rule fired. Checked 0 stream rules.\n");

        let mut stdin = BytesRead {
            bytes: vec![b'x'; MAX_TEST_INPUT_BYTES + 1],
            offset: 0,
        };
        let error = run(
            RulesCommand::Test(RuleTestArgs {
                source: RuleTestSource::Text,
                tool: "patch".to_owned(),
                path: None,
                text: "-".to_owned(),
            }),
            &input,
            dir.path(),
            dir.path(),
            &RulesConfig::default(),
            &mut stdin,
            &mut stdout,
        )
        .await
        .unwrap_err();
        assert_eq!(error.exit_code(), ExitCode::FAILURE);
        assert_eq!(
            error.to_string(),
            "dalgon: rules test input exceeds 1048576 bytes\nPass TEXT directly or pipe at most 1048576 bytes to dalgon rules test."
        );
    }
    #[tokio::test]
    async fn tool_source_reports_match_without_running_tool() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let rules = workspace.path().join(".dal/rules");
        tokio::fs::create_dir_all(&rules).await.unwrap();
        tokio::fs::write(
            rules.join("no-auto-commit.md"),
            "---\ncondition: git commit -m\nscope: tool:exec\n---\nDo not commit automatically.\n",
        )
        .await
        .unwrap();
        let mut stdin = NeverRead(Arc::new(AtomicUsize::new(0)));
        let mut stdout = Vec::new();
        let input = RuleBuildInput {
            records: &[],
            plugin_rules: &[],
            known_tools: &["exec"],
            agent: "test",
        };
        let result = run(
            RulesCommand::Test(RuleTestArgs {
                source: RuleTestSource::Tool,
                tool: "exec".to_owned(),
                path: None,
                text: "git commit -m x".to_owned(),
            }),
            &input,
            data.path(),
            workspace.path(),
            &RulesConfig::default(),
            &mut stdin,
            &mut stdout,
        )
        .await
        .unwrap();

        assert_eq!(result, ExitCode::SUCCESS);
        assert_eq!(
            stdout,
            b"fired: no-auto-commit (interrupt). The `exec` call matched /git commit -m/.\n"
        );
    }

    #[tokio::test]
    async fn report_uses_user_and_workspace_roots_and_surfaces_skipped_rules() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        let user_rules = data.path().join("rules");
        let project_rules = workspace.path().join(".dal/rules");
        let unrelated_rules = unrelated.path().join(".dal/rules");
        tokio::fs::create_dir_all(&user_rules).await.unwrap();
        tokio::fs::create_dir_all(&project_rules).await.unwrap();
        tokio::fs::create_dir_all(&unrelated_rules).await.unwrap();

        let broken = user_rules.join("broken.md");
        let project = project_rules.join("workspace-rule.md");
        tokio::fs::write(&broken, "---\ncondition: git commit -m\n")
            .await
            .unwrap();
        tokio::fs::write(
            &project,
            "---\ncondition: git commit -m\nscope: tool:exec\n---\nCheck project commits.\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            unrelated_rules.join("foreign-rule.md"),
            "---\ncondition: foreign\n---\nMust not load.\n",
        )
        .await
        .unwrap();

        let mut stdin = NeverRead(Arc::new(AtomicUsize::new(0)));
        let mut stdout = Vec::new();
        let input = RuleBuildInput {
            records: &[],
            plugin_rules: &[],
            known_tools: &["exec"],
            agent: "test",
        };
        let result = run(
            RulesCommand::List,
            &input,
            data.path(),
            workspace.path(),
            &RulesConfig::default(),
            &mut stdin,
            &mut stdout,
        )
        .await
        .unwrap();

        assert_eq!(result, ExitCode::FAILURE);
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains(project.to_string_lossy().as_ref()));
        assert!(output.contains(broken.to_string_lossy().as_ref()));
        assert!(!output.contains("foreign-rule"));
        assert!(output.contains("problems (1)\n"));
    }
}
