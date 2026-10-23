//! A task artifact written through the sidecar service lands under the
//! canonical, hyphenated job id: the directory name a reader derives from
//! the job id it was given, published atomically with owner-only permissions.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
    ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef, ToolError};
use dal_core::{
    ArtifactFile, ClientId, Command, Config, ConfigProduct, Expect, JobId, ModelInfo, Name, Origin,
    Part, RawJson, ServiceSet, SidecarOp, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};

const WAIT: Duration = Duration::from_secs(60);

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SCRIPT: &str = concat!(
    "{\"kind\":\"events\",\"events\":[",
    "{\"type\":\"tool_call_started\",\"id\":\"c-1\",\"name\":\"fixture__artifact\"},",
    "{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"c-1\",\"name\":\"fixture__artifact\",",
    "\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},",
    "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,",
    "\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},",
    "{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n",
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},",
    "{\"type\":\"tool_calls_done\",\"calls\":[]},",
    "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,",
    "\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},",
    "{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
);

/// Writes one fixed artifact file for `job` through the sidecar service.
struct ArtifactWriter {
    name: Name,
    spec: Arc<ToolSpec>,
    job: JobId,
    file: ArtifactFile,
    bytes: &'static [u8],
}

impl Tool for ArtifactWriter {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let op = SidecarOp::Artifact {
                job: self.job,
                file: self.file,
                bytes: self.bytes.to_vec(),
            };
            match cx.services().sidecar(cx.caller(), op).await {
                Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("written"))),
                Err(error) => ToolOutcome::Err(ToolError::message(error.to_string())),
            }
        })
    }
}

/// Returns every directory directly under `dir`.
fn children(dir: &Path) -> Result<Vec<std::path::PathBuf>, std::io::Error> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect()
}

/// Runs one real host turn whose tool writes `file` with `bytes` through the
/// sidecar service. Returns the temp root (the caller keeps it alive), the
/// data root, and the job the artifact belongs to.
async fn write_through_host(
    file: ArtifactFile,
    bytes: &'static [u8],
) -> Result<(tempfile::TempDir, std::path::PathBuf, JobId), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, SCRIPT)?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let job = JobId::new_v7();
    let tool = Arc::new(ArtifactWriter {
        name: Name::parse("fixture__artifact")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__artifact")?,
            description: "artifact writer".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
        job,
        file,
        bytes,
    });
    let extension =
        ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::from_names(["sidecar"])?)?
            .with_origin(Origin::Builtin, None)
            .tool(tool, Visibility::Model)
            .build()?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(tmp.path().join("home")),
            ),
        ]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace)?,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let mut updates = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "write the artifact".into(),
            }],
        })
        .await?;
    loop {
        let delivery = tokio::time::timeout(WAIT, updates.next())
            .await?
            .ok_or("the session closed before the turn ended")?;
        if let Delivery::Update(update) = delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            break;
        }
    }

    Ok((tmp, data, job))
}

/// The one session directory the isolation tree holds.
fn only_session(data: &Path) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let sessions = children(&data.join("isolation"))?;
    let [session] = sessions.as_slice() else {
        return Err(format!("expected one session directory, found {sessions:?}").into());
    };
    Ok(session.clone())
}

#[tokio::test]
async fn a_task_artifact_lands_under_the_canonical_job_id() -> TestResult {
    let (_tmp, data, job) = write_through_host(ArtifactFile::DeltaPatch, b"diff").await?;
    let session = only_session(&data)?;
    let tasks = children(&session)?;
    assert_eq!(
        tasks,
        vec![session.join(job.to_string())],
        "the task directory must be the job id a reader is given"
    );
    assert_eq!(
        std::fs::read(session.join(job.to_string()).join("delta.patch"))?,
        b"diff"
    );
    Ok(())
}

/// The salvage status record goes through the same durable owner as the
/// patch: `status.txt` holds exactly the given bytes under the canonical job
/// directory, owner-only on Unix, with no temp file left beside it.
#[tokio::test]
async fn a_status_artifact_lands_privately_and_atomically_under_the_job() -> TestResult {
    const STATUS: &[u8] = b" M seed.txt\nM  staged.txt\n";
    let (_tmp, data, job) = write_through_host(ArtifactFile::StatusTxt, STATUS).await?;
    let task = only_session(&data)?.join(job.to_string());
    assert_eq!(std::fs::read(task.join("status.txt"))?, STATUS);
    let names = std::fs::read_dir(&task)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        names,
        [OsString::from("status.txt")],
        "atomic publication leaves no temp file beside the record"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(task.join("status.txt"))?
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the status record is owner-only like the patch"
        );
    }
    Ok(())
}
