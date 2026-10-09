// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
use gates::support::*;

use std::{io, num::NonZeroU32, time::Duration};

use dal_agent::SessionRef;
use dal_core::{ClientId, Command, Output, PageReq, Reply, Workspace};

fn done_output(reply: Reply) -> support::TestResult<Output> {
    match reply {
        Reply::Done(output) => Ok(output),
        other => Err(io::Error::other(format!("unexpected command reply: {other:?}")).into()),
    }
}

#[test]
fn plan_and_todos_commands_use_a_real_session_journal() -> support::TestResult<()> {
    let scratch = support::Scratch::new("work-session-commands")?;
    let root = scratch.path().to_path_buf();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(root.clone()).await?;
        let workspace = Workspace::new(root)?;
        let agent = host
            .open(
                SessionRef::New {
                    workspace,
                    name: None,
                },
                ClientId::new("dalgona-work-gate"),
            )
            .await?;
        let plan_on = done_output(
            agent
                .submit(Command::Run {
                    name: "plan".into(),
                    args: "on".into(),
                    expected: None,
                })
                .await?,
        )?;
        let plan_off = done_output(
            agent
                .submit(Command::Run {
                    name: "plan".into(),
                    args: "off".into(),
                    expected: None,
                })
                .await?,
        )?;
        assert!(matches!(plan_on, Output::Text(_)));
        assert!(matches!(plan_off, Output::Text(_)));
        assert_ne!(
            plan_on, plan_off,
            "plan mode commands must change session state"
        );

        let todos = done_output(
            agent
                .submit(Command::Run {
                    name: "todos".into(),
                    args: "".into(),
                    expected: None,
                })
                .await?,
        )?;
        assert_eq!(todos, Output::Markdown("No todo list.".into()));
        let page = PageReq::new(NonZeroU32::new(1).ok_or("page size must be nonzero")?, None)?;
        let session = agent.view(page)?.session.id;
        host.close(session).await?;
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
