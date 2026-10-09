//! A task artifact written through the sidecar service lands under the
//! canonical, hyphenated job id: the directory name a reader derives from
//! the job id it was given.

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

/// Writes one `delta.patch` artifact for `job` through the sidecar service.
struct ArtifactWriter {
    name: Name,
    spec: Arc<ToolSpec>,
    job: JobId,
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
                file: ArtifactFile::DeltaPatch,
                bytes: b"diff".to_vec(),
            };
            match cx.services().sidecar(cx.caller(), op).await {
                Ok(_) => ToolOutcome::Ok(ToolOutput::from_text("written")),
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

#[tokio::test]
async fn a_task_artifact_lands_under_the_canonical_job_id() -> TestResult {
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

    let sessions = children(&data.join("isolation"))?;
    let [session] = sessions.as_slice() else {
        return Err(format!("expected one session directory, found {sessions:?}").into());
    };
    let tasks = children(session)?;
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
