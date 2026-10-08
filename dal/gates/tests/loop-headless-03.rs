//! Killed process resume records aborts and replays synthetic results.
#![expect(
    clippy::disallowed_methods,
    reason = "SC test kills a real host process"
)]
#![expect(
    dead_code,
    reason = "gate support exposes helpers shared across independent targets"
)]

mod support;

use std::{
    error::Error,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use support::{TestDir, dalgon_binary};

#[tokio::test]
async fn killed_process_resume_records_abort_and_synthetic_results()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let config_dir = home.join(".config/dal");
    let data_dir = home.join(".local/share");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&workspace)?;
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures");
    fs::copy(
        fixtures.join("process/grandchild.sh"),
        workspace.join("grandchild.sh"),
    )?;
    let replay = fixtures.join("replay/loop-headless.jsonl");
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
            replay.to_string_lossy()
        ),
    )?;
    let binary = dalgon_binary("dalgon")?;
    let mut host = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_dir)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["-p", "--name", "recovery", "Run the grandchild tool."])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid_file = workspace.join("grandchild.pid");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    host.kill()?;
    let _ = host.wait()?;
    let child_pid = fs::read_to_string(&pid_file)?.trim().to_owned();
    #[cfg(unix)]
    let _ = Command::new("kill").args(["-9", &child_pid]).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &child_pid, "/T", "/F"])
        .status();

    let resumed = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_dir)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["-p", "--resume", "recovery", "Continue after recovery."])
        .output()?;
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let journals = collect_journals(&data_dir)?;
    assert!(journals.iter().any(|journal| journal.contains("aborted")));
    assert!(
        journals
            .iter()
            .any(|journal| journal.contains("tool_result"))
    );
    Ok(())
}

fn collect_journals(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut journals = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            journals.extend(collect_journals(&path)?);
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            journals.push(fs::read_to_string(path)?);
        }
    }
    Ok(journals)
}
