//! The smallest dal embedder: start a host, open one ephemeral session, print the answer.
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Part, Reply, Stop, StreamChannel, UpdateKind,
    Workspace,
};
use std::{collections::BTreeMap, io, path::PathBuf, time::Duration};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let workspace = Workspace::new(cwd.clone())?;
    let product = Product {
        name: "dal",
        data_root: cwd.clone(),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/sdk-scripted.jsonl");
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &cwd, "", Some(&user))?;
    let env = Env {
        vars: BTreeMap::new(),
        cwd,
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral { workspace },
            ClientId::new("sdk-example"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the scripted assistant response.".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(io::Error::other("SDK prompt was not accepted").into());
    }
    let mut assistant_text = String::new();
    let stop = loop {
        let Some(delivery) = subscription.next().await else {
            return Err(io::Error::other("SDK subscription closed before the turn ended").into());
        };
        let Delivery::Update(update) = delivery else {
            return Err(io::Error::other("SDK subscription requires a resync").into());
        };
        match &update.kind {
            UpdateKind::Delta {
                channel: StreamChannel::Text,
                text,
                ..
            } => assistant_text.push_str(text),
            UpdateKind::TurnEnded { stop, .. } => break *stop,
            _ => {}
        }
    };
    if stop != Stop::EndTurn {
        return Err(io::Error::other(format!("SDK turn ended with {stop:?}")).into());
    }
    if assistant_text.is_empty() {
        return Err(io::Error::other("SDK turn returned no assistant text").into());
    }
    let report = host.shutdown(Duration::from_secs(5)).await;
    if report.sessions_closed != 1 {
        return Err(io::Error::other("SDK host did not close its session").into());
    }
    println!("{assistant_text}");
    Ok(())
}
