//! A registered extension scheme resolver resolves through the host door,
//! acting as the extension that registered it.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::error::{SchemeError, ToolError};
use dal_agent::ext::{
    ArgError, BoxFuture, Doc, Extension, ExtensionBuilder, RawValue, SchemeCx, SchemeResolver,
    Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Env, Host, Product, ServiceError, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, EntryKind, Expect, JournalPart, ModelInfo, Name,
    Origin, Part, RawJson, ServiceSet, ToolClass, ToolSpec, Visibility, Workspace,
};

const TOOL_NAME: &str = "schemeprobe__resolve";
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn call_step(tool: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-{tool}\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-{tool}\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

fn end_step() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

fn spec(
    name: &str,
    description: &str,
) -> Result<(Name, Arc<ToolSpec>), Box<dyn std::error::Error>> {
    let name = Name::parse(name)?;
    let spec = Arc::new(ToolSpec {
        name: name.clone(),
        description: description.into(),
        parameters: RawJson::parse(r#"{"type":"object"}"#)?,
        grammar: None,
    });
    Ok((name, spec))
}

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
            ToolOutcome::Ok(Box::new(ToolOutput::from_text(format!(
                "{}; unknown={unknown}",
                doc.text
            ))))
        })
    }
}

/// Reports the identity it reads with: the caller's extension, how many
/// `note` records that caller owns, and whether its `env` service answers.
struct OwnedResolver;

impl SchemeResolver for OwnedResolver {
    fn read<'a>(
        &'a self,
        path: &'a str,
        cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<Doc, SchemeError>> {
        Box::pin(async move {
            let caller = cx.caller();
            let services = cx.services();
            let notes = services
                .records(caller, "note")
                .await
                .map_err(|error| SchemeError::Failed {
                    message: error.to_string().into(),
                })?
                .len();
            let env = match services.env(caller, "OPENAI_API_KEY").await {
                Ok(_) => "answered",
                Err(ServiceError::Denied(_)) => "denied",
                Err(error) => {
                    return Err(SchemeError::Failed {
                        message: error.to_string().into(),
                    });
                }
            };
            Ok(Doc::new(
                format!("owned://{path}"),
                format!("ext={}; notes={notes}; env={env}", caller.ext()),
            ))
        })
    }
}

/// Appends `count` `note` records as its own extension, then, when `peek`
/// is set, reads `owned://notes` through the tool door and the services
/// door and reports both texts.
struct NoteTool {
    name: Name,
    spec: Arc<ToolSpec>,
    count: usize,
    peek: bool,
}

impl Tool for NoteTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            for _ in 0..self.count {
                if let Err(error) = services
                    .append_record(cx.caller(), "note", Box::new(call.args.clone()))
                    .await
                {
                    return ToolOutcome::Err(ToolError::message(error.to_string()));
                }
            }
            if !self.peek {
                return ToolOutcome::Ok(Box::new(ToolOutput::from_text("noted")));
            }
            let tool = match cx.resolve("owned://notes").await {
                Ok(doc) => doc.text.to_string(),
                Err(error) => return ToolOutcome::Err(error),
            };
            let service = match services.scheme(cx.caller(), "owned://notes").await {
                Ok(Some(doc)) => doc.text.to_string(),
                Ok(None) => "absent".to_owned(),
                Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
            };
            ToolOutcome::Ok(Box::new(ToolOutput::from_text(format!(
                "tool: {tool} | service: {service}"
            ))))
        })
    }
}

/// Runs one scripted prompt turn against `extensions` and returns the
/// successful text result of `tool`.
async fn drive(
    script: &str,
    extensions: Vec<Extension>,
    tool: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, script)?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let workspace = Workspace::new(workspace_dir)?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("scheme-probe"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "resolve a scheme".into(),
            }],
        })
        .await?;
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
    let view = agent.view(dal_core::PageReq::default())?;
    Ok(view
        .entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::ToolResult {
                name,
                error: false,
                parts,
                ..
            } if name.as_ref() == tool => parts.iter().find_map(|part| match part {
                JournalPart::Text { text } => Some(text.to_string()),
                _ => None,
            }),
            _ => None,
        }))
}

#[tokio::test]
async fn registered_schemes_resolve_and_unknown_schemes_are_not_found() -> TestResult {
    let (name, spec) = spec(TOOL_NAME, "resolve custom schemes")?;
    let extension = ExtensionBuilder::new("schemeprobe", "0.1.0", ServiceSet::EMPTY)?
        .tool(Arc::new(ResolveTool { name, spec }), Visibility::Model)
        .scheme("custom", Arc::new(CustomResolver))
        .build()?;
    let script = format!("{}{}", call_step(TOOL_NAME), end_step());
    let result = drive(&script, vec![extension], TOOL_NAME).await?;
    assert_eq!(result.as_deref(), Some("resolved path; unknown=NotFound"));
    Ok(())
}

/// A resolver reads as the extension that registered it: it sees its
/// owner's records, not the requester's, and it never borrows the
/// requester's injected services or builtin grant shortcut.
#[tokio::test]
async fn a_scheme_resolver_reads_as_its_owner_and_never_as_the_requester() -> TestResult {
    let (owner_tool, owner_spec) = spec("note", "record owner notes")?;
    let owner = ExtensionBuilder::new("noteowner", "0.1.0", ServiceSet::EMPTY)?
        .tool(
            Arc::new(NoteTool {
                name: owner_tool,
                spec: owner_spec,
                count: 2,
                peek: false,
            }),
            Visibility::Model,
        )
        .scheme("owned", Arc::new(OwnedResolver))
        .build()?;
    let (reader_tool, reader_spec) = spec("peek", "record one note and read the owner scheme")?;
    let reader = ExtensionBuilder::new("notereader", "0.1.0", ServiceSet::from_names(["env"])?)?
        .with_origin(Origin::Builtin, None)
        .tool(
            Arc::new(NoteTool {
                name: reader_tool,
                spec: reader_spec,
                count: 1,
                peek: true,
            }),
            Visibility::Model,
        )
        .build()?;
    let script = format!("{}{}{}", call_step("note"), call_step("peek"), end_step());
    let result = drive(&script, vec![owner, reader], "peek").await?;
    let expected_output = "ext=noteowner; notes=2; env=denied";
    assert_eq!(
        result.as_deref(),
        Some(format!("tool: {expected_output} | service: {expected_output}").as_str())
    );
    Ok(())
}
