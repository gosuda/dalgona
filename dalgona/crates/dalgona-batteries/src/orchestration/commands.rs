// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::sync::Arc;

use dal_agent::error::ServiceError;
use dal_agent::ext::{BoxFuture, CommandCx, CommandHandler, ExtensionBuilder};
use dal_core::{CommandName, CommandSpec, Output, RegistrationError, Reply};

use super::runtime::Runtime;

struct OrchestrationCommand {
    name: &'static str,
    runtime: Runtime,
}

impl CommandHandler for OrchestrationCommand {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let text = self.runtime.command(cx.session(), self.name, args).await?;
            Ok(Reply::Done(Output::Text(text.into())))
        })
    }
}

fn add_command(
    builder: ExtensionBuilder,
    runtime: &Runtime,
    name: &'static str,
    summary: &'static str,
    args_hint: Option<&'static str>,
) -> Result<ExtensionBuilder, RegistrationError> {
    let spec = CommandSpec {
        name: CommandName::parse(name)?,
        summary: summary.into(),
        args_hint: args_hint.map(Into::into),
    };
    Ok(builder.command(
        spec,
        Arc::new(OrchestrationCommand {
            name,
            runtime: runtime.clone(),
        }),
    ))
}

pub(crate) fn register(
    mut builder: ExtensionBuilder,
    runtime: &Runtime,
) -> Result<ExtensionBuilder, RegistrationError> {
    let config = runtime.config();
    if config.goal.enabled {
        builder = add_command(
            builder,
            runtime,
            "goal",
            "Show or change the session goal",
            Some("<objective|pause|resume|clear>"),
        )?;
    }
    if config.arbiter.enabled {
        builder = add_command(
            builder,
            runtime,
            "continuation",
            "Control automatic turns",
            Some("<run|pause|stop>"),
        )?;
    }
    if config.arbiter.enabled || config.monitor.enabled {
        builder = add_command(
            builder,
            runtime,
            "abort",
            "Cancel orchestration work and pause automatic turns",
            None,
        )?;
    }
    Ok(builder)
}
