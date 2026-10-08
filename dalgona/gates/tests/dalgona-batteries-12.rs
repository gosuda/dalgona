// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::{io, num::NonZeroU32, time::Duration};

use dal_agent::SessionRef;
use dal_core::{ClientId, Command, Config, ConfigProduct, Output, PageReq, Reply, Workspace};

#[test]
fn empty_mcp_declarations_start_no_transport_or_mapped_tool() -> support::TestResult<()> {
    let scratch = support::Scratch::new("inert-web-mcp")?;
    let root = scratch.path().to_path_buf();
    let factory = dalgona::product();
    let config = Config::load(ConfigProduct::Dalgona, &root, factory.defaults, None)?;
    let cx = dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    };
    let product = dalgona::build(&cx)?;
    let mcp = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "mcp")
        .ok_or_else(|| io::Error::other("the product has no mcp extension"))?;
    assert!(mcp.tools().is_empty(), "MCP tools are session-declared");
    assert_eq!(mcp.mcp_clients().len(), 1);
    assert!(
        !root.join("mcp").exists(),
        "registration starts no transport"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(root.clone()).await?;
        assert!(
            host.commands()
                .iter()
                .any(|command| command.name.as_str() == "mcp"),
            "the /mcp status command is always registered"
        );
        let workspace = Workspace::new(root.clone())?;
        let agent = host
            .open(
                SessionRef::New {
                    workspace,
                    name: None,
                },
                ClientId::new("mcp-empty-gate"),
            )
            .await?;
        let reply = agent
            .submit(Command::Run {
                name: "mcp".into(),
                args: "".into(),
                expected: None,
            })
            .await?;
        assert_eq!(
            reply,
            Reply::Done(Output::Markdown("no MCP servers in this session.".into()))
        );
        assert!(
            !host
                .commands()
                .iter()
                .any(|command| command.name.as_str() == "web_search"),
            "web_search is a tool, not a slash command"
        );
        let page = PageReq::new(NonZeroU32::new(1).ok_or("page size must be nonzero")?, None)?;
        let session = agent.view(page)?.session.id;
        host.close(session).await?;
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    assert!(
        !root.join("mcp").exists(),
        "empty declarations start no server or token store"
    );
    Ok(())
}
