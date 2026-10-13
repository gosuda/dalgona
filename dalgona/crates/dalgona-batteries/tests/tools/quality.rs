// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::common::{Host as ScriptedServices, TestResult, locked, run_tool};
use dal_agent::ext::{BoxFuture, ExtensionBuilder, HookCx, HookError, ObserveHook};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::SessionStart;
use dal_core::{
    ApprovalMode, ClientId, Command, Config, ConfigProduct, Expect, Part, RawJson, Save,
    ServiceSet, UpdateKind, Workspace,
};
use dal_tools::guard::{GuardConfig, guard_extension};
use dalgona_batteries::quality::{QualityConfig, offers::Offer, quality};

fn extension() -> Result<dal_agent::ext::Extension, Box<dyn StdError>> {
    let guard = guard_extension(GuardConfig::disabled())?;
    Ok(quality(
        QualityConfig {
            detectors_enabled: false,
        },
        guard.findings,
        guard.observer,
    )?)
}

async fn blocking<T, F>(operation: F) -> Result<T, Box<dyn StdError>>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    Ok(tokio::task::spawn_blocking(operation).await??)
}

async fn temp_file(label: &str, bytes: &[u8]) -> Result<(String, PathBuf), Box<dyn StdError>> {
    let name = format!("dalgona-quality-{label}-{}.py", std::process::id());
    let path = std::env::temp_dir().join(&name);
    let write_path = path.clone();
    let bytes = bytes.to_vec();
    blocking(move || std::fs::write(write_path, bytes)).await?;
    Ok((name, path))
}

async fn read_and_remove(path: PathBuf) -> Result<Vec<u8>, Box<dyn StdError>> {
    blocking(move || {
        let bytes = std::fs::read(&path)?;
        std::fs::remove_file(path)?;
        Ok(bytes)
    })
    .await
}

#[derive(serde::Serialize)]
struct OfferEnvelope<'a> {
    turn: u64,
    offers: &'a [Offer],
}

fn offer_envelope(offer: &Offer) -> OfferEnvelope<'_> {
    OfferEnvelope {
        turn: 1,
        offers: std::slice::from_ref(offer),
    }
}

fn store_offer(host: &ScriptedServices, offer: &Offer) -> Result<(), Box<dyn StdError>> {
    let encoded = sonic_rs::to_string(&offer_envelope(offer))?;
    let body = RawJson::parse(&encoded)?;
    locked(&host.records)
        .entry("quality_offers".to_owned())
        .or_default()
        .push(body);
    Ok(())
}

#[tokio::test]
async fn quality_apply_rejects_a_stale_offer_without_writing() -> TestResult {
    let host = ScriptedServices::answering([]);
    let (name, path) = temp_file("stale", b"header\n# new code\n").await?;
    store_offer(
        &host,
        &Offer {
            id: "q-1-1".to_owned(),
            codemod: "delete-commented-code".to_owned(),
            path: name.clone(),
            byte_start: 7,
            byte_end: 18,
            line_start: 2,
            line_end: 2,
            before: "# old code\n".to_owned(),
            after: String::new(),
        },
    )?;

    let result = run_tool(&extension()?, &host, "quality_apply", r#"{"id":"q-1-1"}"#).await;
    let on_disk = read_and_remove(path).await?;
    assert_eq!(
        result?,
        format!("quality_apply: {name} changed since the offer; the offer is stale.")
    );
    assert_eq!(on_disk, b"header\n# new code\n");
    Ok(())
}

#[tokio::test]
async fn quality_apply_leaves_the_file_untouched_when_approval_is_denied() -> TestResult {
    let host = ScriptedServices::answering([]);
    let original = b"header\n# old code\nkeep = 1\n";
    let (name, path) = temp_file("denied", original).await?;
    store_offer(
        &host,
        &Offer {
            id: "q-1-1".to_owned(),
            codemod: "delete-commented-code".to_owned(),
            path: name,
            byte_start: 7,
            byte_end: 18,
            line_start: 2,
            line_end: 2,
            before: "# old code\n".to_owned(),
            after: String::new(),
        },
    )?;

    let result = run_tool(&extension()?, &host, "quality_apply", r#"{"id":"q-1-1"}"#).await;
    let on_disk = read_and_remove(path).await?;
    let text = result?;
    assert_eq!(text, "denied: no front end can answer");
    assert_eq!(on_disk, original);
    Ok(())
}

fn quality_extension_with_offer(
    offer: Offer,
) -> Result<dal_agent::ext::Extension, Box<dyn StdError>> {
    let base = extension()?;
    let services = ServiceSet::from_names(["fs.read", "fs.write"])?;
    let mut builder = ExtensionBuilder::new("quality", "0.1.0", services)?;
    for (tool, visibility) in base.tools() {
        builder = builder.tool(Arc::clone(tool), *visibility);
    }
    Ok(builder.on_session_start(SeedOffer(offer)).build()?)
}

struct SeedOffer(Offer);

impl ObserveHook<SessionStart> for SeedOffer {
    fn call(&self, _input: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let offer = self.0.clone();
        Box::pin(async move {
            let encoded = sonic_rs::to_string(&offer_envelope(&offer)).map_err(|error| {
                HookError::Failed {
                    message: error.to_string().into(),
                }
            })?;
            let body = RawJson::parse(&encoded).map_err(|error| HookError::Failed {
                message: error.to_string().into(),
            })?;
            cx.services
                .append_record(&cx.caller, "quality_offers", Box::new(body))
                .await
                .map(|_| ())
                .map_err(|error| HookError::Failed {
                    message: error.to_string().into(),
                })
        })
    }
}

fn scripted_fixture() -> String {
    concat!(
        r#"{"kind":"events","events":[{"type":"tool_call_started","id":"c-quality_apply","name":"quality_apply"},{"type":"tool_calls_done","calls":[{"id":"c-quality_apply","name":"quality_apply","args":{"kind":"parsed","value":{"id":"q-1-1"}}}]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}"#,
        "\n",
        r#"{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#,
        "\n"
    )
    .to_owned()
}

fn prepare_quality_session_files(
    data: PathBuf,
    source: PathBuf,
    fixture: PathBuf,
    scripted: String,
) -> io::Result<()> {
    std::fs::create_dir_all(data)?;
    std::fs::create_dir_all(
        source
            .parent()
            .ok_or_else(|| io::Error::other("source parent missing"))?,
    )?;
    std::fs::write(source, b"header\n# old code\nkeep = 1\n")?;
    std::fs::write(fixture, scripted)
}

async fn quality_session_config(
    data: PathBuf,
    fixture: &Path,
) -> Result<Config, Box<dyn StdError>> {
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    Ok(tokio::task::spawn_blocking(move || {
        Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))
    })
    .await??)
}

#[tokio::test]
async fn quality_apply_commits_the_approved_offer_in_a_real_session() -> TestResult {
    let root = std::env::temp_dir().join(format!("dalgona-quality-session-{}", std::process::id()));
    let data = root.join("data");
    let workspace_dir = root.join("workspace");
    let source = workspace_dir.join("src/a.py");
    let fixture = data.join("script.jsonl");
    let scripted = scripted_fixture();
    blocking({
        let setup_data = data.clone();
        let setup_source = source.clone();
        let setup_fixture = fixture.clone();
        move || prepare_quality_session_files(setup_data, setup_source, setup_fixture, scripted)
    })
    .await?;
    let config = quality_session_config(data.clone(), &fixture).await?;
    let quality = quality_extension_with_offer(Offer {
        id: "q-1-1".to_owned(),
        codemod: "delete-commented-code".to_owned(),
        path: "src/a.py".to_owned(),
        byte_start: 7,
        byte_end: 18,
        line_start: 2,
        line_end: 2,
        before: "# old code\n".to_owned(),
        after: String::new(),
    })?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![quality],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir)?,
                name: None,
            },
            ClientId::new("quality-apply"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        })
        .await?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "apply the offered edit".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, dal_core::Reply::Accepted { .. }));
    let mut ended = false;
    for _ in 0..20 {
        let delivery = tokio::time::timeout(Duration::from_secs(30), subscription.next()).await?;
        let Some(delivery) = delivery else {
            break;
        };
        if let dal_agent::Delivery::Update(update) = delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            ended = true;
            break;
        }
    }
    let updated = blocking({
        let source = source.clone();
        move || std::fs::read(source)
    })
    .await?;
    drop(subscription);
    drop(agent);
    drop(host);
    blocking(move || std::fs::remove_dir_all(root)).await?;
    assert!(ended, "the scripted tool call did not finish the turn");
    assert_eq!(updated, b"header\nkeep = 1\n");
    Ok(())
}
