#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]
//! Exercises eval cell service injection and EvalOnly tool visibility.

mod support;

use std::{collections::BTreeMap, error::Error, ffi::OsString, fs, io, time::Duration};

use dal_agent::{Agent, Delivery, Env, SessionRef, Subscription};
use dal_core::{
    Answer, Command, Config, ConfigProduct, EntryKind, EntryView, Expect, JournalPart, Part,
    Question, Reply, Stop, UpdateKind, View, Workspace,
};
use support::{TestDir, scripted_session};

const PLUGIN: &str = r#"load("@dal/v1", "dal")

def cell_only(ctx, args):
    return args

cell_tool = dal.tool(
    description = "Available to eval cells only.",
    input = dal.schema(value = dal.string()),
    uses = [],
    visibility = "eval_only",
    run = cell_only,
)

plugin = dal.plugin(
    name = "gate",
    version = "0.1.0",
    tools = {"cell_only": cell_tool},
)
"#;

fn eval_args(code: &str, uses: &str) -> String {
    let encoded = sonic_rs::to_string(code).unwrap();
    format!(r#"{{"code":{encoded},"uses":{uses}}}"#)
}

fn tool_call_response(id: &str, tool: &str, args: &str) -> String {
    let mut line = String::from(r#"{"kind":"events","events":[{"type":"tool_call_started","id":""#);
    line.push_str(id);
    line.push_str(r#"","name":""#);
    line.push_str(tool);
    line.push_str(r#""},{"type":"tool_calls_done","calls":[{"id":""#);
    line.push_str(id);
    line.push_str(r#"","name":""#);
    line.push_str(tool);
    line.push_str(r#"","args":{"kind":"parsed","value":"#);
    line.push_str(args);
    line.push_str(r#"}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}"#);
    line
}

fn text_response(text: &str) -> String {
    let encoded = sonic_rs::to_string(text).expect("fixture response encodes as JSON");
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":{encoded}}},{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}}},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

async fn prompt_turn(
    agent: &Agent,
    subscription: &mut Subscription,
    text: &str,
    label: &str,
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(io::Error::other("scripted prompt was not accepted").into());
    }
    let mut requests = 0;
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
            .await
            .map_err(|_| {
                io::Error::other(format!(
                    "{label} turn timed out waiting for a session update"
                ))
            })?;
        let Some(delivery) = delivery else {
            return Err(io::Error::other("session subscription closed before turn end").into());
        };
        let Delivery::Update(update) = delivery else {
            continue;
        };
        eprintln!("{label}: {:?}", update.kind);
        if let UpdateKind::RequestOpened(request) = &update.kind {
            requests += 1;
            if !matches!(&request.question, Question::Approval { tool, .. } if tool.as_ref() == "eval")
            {
                return Err(io::Error::other("unexpected ask request from eval cell").into());
            }
            agent.answer(request.id, Answer::Approve).await?;
        }
        let UpdateKind::TurnEnded { stop, .. } = &update.kind else {
            continue;
        };
        if *stop == Stop::EndTurn {
            return Ok(requests);
        }
        return Err(io::Error::other(format!(
            "eval turn ended before its cell completed: {stop:?}"
        ))
        .into());
    }
}

fn last_tool_result(entries: &[EntryView], tool: &str) -> Option<(bool, String)> {
    entries.iter().rev().find_map(|entry| {
        let EntryKind::ToolResult {
            name, error, parts, ..
        } = &entry.kind
        else {
            return None;
        };
        if name.as_ref() != tool {
            return None;
        }
        let text = parts
            .iter()
            .filter_map(|part| match part {
                JournalPart::Text { text } => Some(text.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some((*error, text))
    })
}

fn shell_environment() -> BTreeMap<OsString, OsString> {
    std::env::vars_os()
        .filter(|(key, _)| {
            key.to_str().is_some_and(|name| {
                ["PATH", "ProgramFiles", "ProgramFiles(x86)"]
                    .iter()
                    .any(|expected| name.eq_ignore_ascii_case(expected))
            })
        })
        .collect()
}

fn assert_eval_completed(view: &View) {
    let Some((error, text)) = last_tool_result(&view.entries.items, "eval") else {
        panic!("the latest eval cell must have a journaled tool result");
    };
    eprintln!("eval result: error={error} text={text}");
    assert!(!error, "the eval cell failed: {text}");
    assert!(text.contains("\"status\":\"completed\""), "{text}");
}

#[tokio::test]
async fn eval_cell_respects_inject_and_eval_only_visibility()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let plugin_dir = data.path().join("plugins/gate");
    fs::create_dir_all(&plugin_dir)?;
    fs::write(plugin_dir.join("plugin.star"), PLUGIN)?;
    let replay = data.path().join("eval-scripted.jsonl");
    let responses = [
        tool_call_response("empty-cell", "eval", &eval_args("1 + 1", "[]")),
        text_response("empty cell completed"),
        tool_call_response(
            "run-cell",
            "eval",
            &eval_args(
                r#"cell_value = tools["gate.cell_only"](value = "from eval")
ran = ctx.tools.exec(command = "true")
[cell_value, ran]"#,
                r#"["tools.exec","tools.gate.cell_only"]"#,
            ),
        ),
        text_response("run cell completed"),
        tool_call_response(
            "model-eval-only",
            "gate__cell_only",
            r#"{"value":"from model"}"#,
        ),
        text_response("model call rejected"),
    ]
    .join("\n");
    fs::write(&replay, format!("{responses}\n"))?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai/gpt-6\"\nplugins = [\"gate\"]\n[eval]\nuses = [\"tools.exec\", \"tools.gate.cell_only\"]\n[providers.scripted]\nfixture = {:?}\n",
        replay.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: shell_environment(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut subscription = harness.agent.subscribe(None)?;

    let empty_requests = prompt_turn(
        &harness.agent,
        &mut subscription,
        "Run a pure cell.",
        "empty",
    )
    .await?;
    assert_eq!(empty_requests, 0);
    assert_eval_completed(&harness.agent.view(dal_core::PageReq::default())?);

    let run_requests = prompt_turn(
        &harness.agent,
        &mut subscription,
        "Run an exec cell.",
        "run",
    )
    .await?;
    let run_result = last_tool_result(
        &harness
            .agent
            .view(dal_core::PageReq::default())?
            .entries
            .items,
        "eval",
    );
    assert_eq!(run_requests, 1, "exec cell result: {run_result:?}");
    assert_eval_completed(&harness.agent.view(dal_core::PageReq::default())?);

    let model_requests = prompt_turn(
        &harness.agent,
        &mut subscription,
        "Call the hidden tool from the model.",
        "model",
    )
    .await?;
    assert_eq!(model_requests, 0);
    let view = harness.agent.view(dal_core::PageReq::default())?;
    let Some((error, text)) = last_tool_result(&view.entries.items, "gate__cell_only") else {
        panic!("the model attempt must produce a tool result");
    };
    assert!(error, "the EvalOnly model call unexpectedly succeeded");
    assert_eq!(text, "gate__cell_only is callable only from eval cells");
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
