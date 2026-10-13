//! Integration coverage for the host documentation seam.
use std::collections::BTreeMap;

use dal_agent::{Env, Host, Product};
use dal_core::{Config, ConfigProduct};

struct TestScheme;

impl dal_agent::ext::SchemeResolver for TestScheme {
    fn read<'a>(
        &'a self,
        path: &'a str,
        _cx: &'a dal_agent::ext::SchemeCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<dal_agent::ext::Doc, dal_agent::error::SchemeError>>
    {
        Box::pin(async move { Ok(dal_agent::ext::Doc::new(format!("test://{path}"), "body")) })
    }
}

#[tokio::test]
async fn docs_list_and_page_read_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
    let extension =
        dal_agent::ext::ExtensionBuilder::new("test", "0.1.0", dal_core::ServiceSet::default())
            .expect("builder")
            .scheme("test", std::sync::Arc::new(TestScheme))
            .doc("protocol", "Protocol", "body")
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
        vars: BTreeMap::new(),
        cwd: data.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let entries = host.docs();
    assert_eq!(entries.len(), 1, "one record listed");
    assert_eq!(entries[0].uri.as_ref(), "test://protocol");
    assert_eq!(entries[0].title.as_ref(), "Protocol");
    let doc = host.doc("test://protocol").expect("page reads back");
    assert_eq!(doc.text.as_ref(), "body");
}
