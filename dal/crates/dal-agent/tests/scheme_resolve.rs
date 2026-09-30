//! A registered extension scheme resolver resolves through the host door.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::error::{SchemeError, ToolError};
use dal_agent::ext::{
    ArgError, BoxFuture, Doc, ExtensionBuilder, RawValue, SchemeCx, SchemeResolver, Tool, ToolCall,
    ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, EntryKind, Expect, JournalPart, ModelInfo, Name,
    Part, RawJson, ServiceSet, ToolClass, ToolSpec, Visibility, Workspace,
};

const TOOL_NAME: &str = "schemeprobe__resolve";
const FIXTURE: &str = concat!(
    r#"{"kind":"events","events":[{"type":"tool_call_started","id":"scheme-call","name":"schemeprobe__resolve"},{"type":"tool_calls_done","calls":[{"id":"scheme-call","name":"schemeprobe__resolve","args":{"kind":"parsed","value":{}}}]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}"#,
    "\n",
    r#"{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#,
    "\n"
);

struct CustomResolver;

impl SchemeResolver for CustomResolver {
    fn read<'a>(
        &'a self,
        path: &'a str,
        _cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<Doc, SchemeError>> {
        let uri = format!("custom://{path}");
        let text = format!("resolved {path}");
        Box::pin(async move { Ok(Doc::new(uri, text)) })
    }
}

struct ResolveTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for ResolveTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let doc = match cx.resolve("custom://path").await {
                Ok(doc) => doc,
                Err(error) => return ToolOutcome::Err(error),
            };
            let unknown = match cx.resolve("missing://path").await {
                Err(ToolError::Scheme(SchemeError::NotFound { uri }))
                    if uri.as_ref() == "missing://path" =>
                {
                    "NotFound"
                }
                Err(error) => return ToolOutcome::Err(error),
                Ok(doc) => {
                    return ToolOutcome::Err(ToolError::Message {
                        message: format!("unknown scheme unexpectedly resolved to {}", doc.uri)
                            .into(),
                    });
                }
            };
            ToolOutcome::Ok(ToolOutput::from_text(format!(
                "{}; unknown={unknown}",
                doc.text
            )))
        })
    }
}

#[tokio::test]
async fn registered_schemes_resolve_and_unknown_schemes_are_not_found() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, FIXTURE).expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let tool = Arc::new(ResolveTool {
        name: Name::parse(TOOL_NAME).expect("tool name"),
        spec: Arc::new(ToolSpec {
            name: Name::parse(TOOL_NAME).expect("tool name"),
            description: "resolve custom schemes".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
            grammar: None,
        }),
    });
    let extension = ExtensionBuilder::new("schemeprobe", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .tool(tool, Visibility::Model)
        .scheme("custom", Arc::new(CustomResolver))
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("scheme-probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "resolve a scheme".into(),
            }],
        })
        .await
        .expect("submit");
    assert!(matches!(reply, dal_core::Reply::Accepted { .. }));
    let mut ended = false;
    for _ in 0..20 {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await;
        let Ok(Some(delivery)) = delivery else {
            break;
        };
        if matches!(
            delivery,
            dal_agent::Delivery::Update(update)
                if matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. })
        ) {
            ended = true;
            break;
        }
    }
    assert!(ended, "scripted resolver turn reaches TurnEnded");
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let result = view
        .entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::ToolResult {
                name,
                error: false,
                parts,
                ..
            } if name.as_ref() == TOOL_NAME => parts.iter().find_map(|part| match part {
                JournalPart::Text { text } => Some(text.as_ref()),
                _ => None,
            }),
            _ => None,
        });
    assert_eq!(result, Some("resolved path; unknown=NotFound"));
}
