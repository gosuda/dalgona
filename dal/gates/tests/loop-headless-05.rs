//! A second product process reports the current session lock holder.
#![expect(
    clippy::disallowed_methods,
    reason = "SC test starts a second product process"
)]
#![expect(
    dead_code,
    reason = "gate support exposes helpers shared across independent targets"
)]

mod support;

use std::{collections::BTreeMap, error::Error, fs, process::Command};

use dal_agent::{Env, Host, SessionRef};
use dal_core::{Config, ConfigProduct, PageReq, Workspace};
use support::TestDir;

#[tokio::test]
async fn second_process_reports_current_session_lock_holder()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let config_dir = home.join(".config/dal");
    let data_home = home.join(".local/share");
    let data_root = data_home.join("dal");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&data_root)?;
    fs::create_dir_all(&workspace)?;
    let factory = dalgon::product();
    let user = "model = \"openai-responses/gpt-6\"\napproval = \"ask\"\n";
    fs::write(config_dir.join("dal.toml"), user)?;
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data_root,
        factory.defaults,
        Some(user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data_root.clone(),
        config: &config,
    })?;
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let workspace_ref = Workspace::new(workspace.clone())?;
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace_ref,
                name: Some("locked-session".into()),
            },
            dal_core::ClientId::new("core"),
        )
        .await?;
    let session_id = agent.view(PageReq::default())?.session.id.to_string();

    let output = Command::new(support::dalgon_binary("dalgon")?)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["-p", "--resume", &session_id, "This must not start."])
        .output()?;
    let expected = format!(
        "session {session_id} is open in process {}",
        std::process::id()
    );
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains(&expected));
    let report = host.shutdown(std::time::Duration::from_secs(1)).await;
    assert_eq!(report.sessions_closed, 1);
    Ok(())
}
