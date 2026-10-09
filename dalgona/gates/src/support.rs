//! Shared helpers for the gate test binaries.
// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
pub type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    pub fn new(label: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn run_command(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Output> {
    command.output()
}

pub const BATTERIES: [&str; 11] = [
    "ask",
    "history",
    "judged",
    "mcp",
    "orchestration",
    "quality",
    "review",
    "skills",
    "ttsr-rules",
    "web",
    "work",
];

pub fn build_product(
    root: PathBuf,
    user_toml: Option<&str>,
) -> TestResult<dal_agent::Product> {
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        user_toml,
    )?;
    let cx = dalgon::BuildCx {
        data_root: root,
        config: &config,
    };
    Ok(dalgona::build(&cx)?)
}

pub fn battery_names(product: &dal_agent::Product) -> std::collections::BTreeSet<&str> {
    product
        .extensions
        .iter()
        .map(dal_agent::ext::Extension::name)
        .filter(|name| BATTERIES.contains(name))
        .collect()
}

pub fn dalgona_binary() -> TestResult<PathBuf> {
    let suffix = std::env::consts::EXE_SUFFIX;
    let exe = std::env::current_exe()?;
    let target_profile = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("the test binary has no target directory")?;
    let path = target_profile.join(format!("dalgona{suffix}"));
    if path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "the Dalgona binary is missing at {}; run `cargo build -p dalgona --bin dalgona` before this gate",
        path.display()
    )
    .into())
}

pub async fn start_dalgona(root: PathBuf) -> TestResult<dal_agent::Host> {
    start_dalgona_with_config(root, None).await
}

pub async fn start_dalgona_with_config(
    root: PathBuf,
    user_toml: Option<&str>,
) -> TestResult<dal_agent::Host> {
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        user_toml,
    )?;
    let cx = dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    };
    let product = (factory.build)(&cx)?;
    let env = dal_agent::Env {
        vars: BTreeMap::new(),
        cwd: root.clone(),
        sandbox_helper: None,
    };
    Ok(dal_agent::Host::start(product, config, env).await?)
}
