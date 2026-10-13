use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dal_core::{ClientId, Config, ConfigProduct, ServiceSet, Workspace};

use crate::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
use crate::{Env, Host, Product, SessionRef};

struct NeverQuiet;

impl StatusPoll for NeverQuiet {
    fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
        StatusSnapshot {
            quiet: false,
            text: Some("still working".into()),
        }
    }
}

#[tokio::test]
async fn service_path_checks() {
    let duplicate = ExtensionBuilder::new("focus", "0.1.0", ServiceSet::EMPTY)
        .expect("extension name")
        .status_kind("one", Arc::new(NeverQuiet))
        .status_kind("two", Arc::new(NeverQuiet))
        .build()
        .expect_err("a second status kind is rejected");
    assert_eq!(
        duplicate.to_string(),
        "extension \"focus\" may register only one status kind"
    );

    let temp = tempfile::tempdir().expect("temporary root");
    let data = temp.path().join("data");
    let workspace_path = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data root");
    std::fs::create_dir_all(&workspace_path).expect("workspace");
    let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
    let extension = ExtensionBuilder::new("focus", "0.1.0", ServiceSet::EMPTY)
        .expect("extension name")
        .status_kind("focus", Arc::new(NeverQuiet))
        .build()
        .expect("status extension");
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: vec![extension],
            bundled: Vec::new(),
        },
        config,
        Env {
            vars: BTreeMap::<OsString, OsString>::new(),
            cwd: workspace_path.clone(),
            sandbox_helper: None,
        },
    )
    .await
    .expect("host starts");
    host.open(
        SessionRef::Ephemeral {
            workspace: Workspace::new(workspace_path).expect("workspace"),
        },
        ClientId::new("service-path-checks"),
    )
    .await
    .expect("session opens");

    let started = Instant::now();
    let report = host.shutdown(Duration::from_millis(150)).await;
    assert!(
        !report.status_quiet,
        "the never-quiet status reaches the grace deadline"
    );
    assert!(started.elapsed() >= Duration::from_millis(150));
}
