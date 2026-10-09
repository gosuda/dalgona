// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::sync::Arc;

use dal_agent::error::{ServiceError, ToolError};
use dal_agent::ext::{
    ArgError, BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
    ToolOutput,
};
use dal_core::{
    DenyReason, ModelInfo, Name, RawJson, RegistrationError, ToolClass, ToolSpec, Visibility,
    Workspace,
};

use super::agents_tool::{AGENTS_DESCRIPTION, AGENTS_SCHEMA, REPORT_DESCRIPTION, REPORT_SCHEMA};
use super::goal::ops::{
    CREATE_GOAL_DESCRIPTION, CREATE_GOAL_SCHEMA, GET_GOAL_DESCRIPTION, GET_GOAL_SCHEMA,
    UPDATE_GOAL_DESCRIPTION, UPDATE_GOAL_SCHEMA,
};
use super::monitor::state::{MONITOR_DESCRIPTION, MONITOR_SCHEMA};
use super::runtime::Runtime;

struct OrchestrationTool {
    name: Name,
    spec: Arc<ToolSpec>,
    runtime: Runtime,
}

impl Tool for OrchestrationTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawValue, workspace: &Workspace) -> Result<ToolClass, ArgError> {
        if self.name.as_str() != "agents" {
            return Ok(ToolClass::Other);
        }
        let action = self.decode(args)?;
        Ok(super::agents_tool::approval_class(
            &action,
            workspace.as_path(),
            self.runtime.config().data_root.as_deref(),
        ))
    }

    fn run<'a>(&'a self, call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            if let Err(reason) = self.approve(&call.args, &mut cx).await {
                return ToolOutcome::Err(ToolError::Denied(reason));
            }
            match self
                .runtime
                .tool(
                    cx.session(),
                    cx.caller().clone(),
                    call.id,
                    self.name.as_str(),
                    call.args,
                    cx.cancel().clone(),
                )
                .await
            {
                Ok(text) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(text.into_boxed_str()))),
                Err(error) => service_failure(error),
            }
        })
    }
}

impl OrchestrationTool {
    /// Decodes one `agents` call against the configured saved workflows.
    fn decode(&self, args: &RawValue) -> Result<super::agents_tool::AgentAction, ArgError> {
        let saved = self
            .runtime
            .config()
            .workflows
            .as_ref()
            .and_then(|workflows| sonic_rs::to_string(workflows).ok())
            .and_then(|text| RawJson::parse(&text).ok());
        super::agents_tool::decode_action(args, saved.as_ref())
            .map_err(|error| ArgError::message(error.to_string()))
    }

    /// Asks once for an `agents run` that is not read-only. The approved
    /// call carries the git grant the run's own `run` calls ride on.
    /// A call that does not decode goes on to the runtime, which words the
    /// refusal.
    async fn approve(&self, args: &RawValue, cx: &mut ToolCx<'_>) -> Result<(), DenyReason> {
        if self.name.as_str() != "agents" {
            return Ok(());
        }
        let Ok(action) = self.decode(args) else {
            return Ok(());
        };
        let super::agents_tool::AgentAction::Run {
            label, workflow, ..
        } = &action
        else {
            return Ok(());
        };
        let class = super::agents_tool::approval_class(
            &action,
            cx.workspace().as_path(),
            self.runtime.config().data_root.as_deref(),
        );
        if matches!(class, ToolClass::Read) {
            return Ok(());
        }
        cx.authorize(super::agents_tool::approval_preview(label, workflow))
            .await
            .map(drop)
    }
}

fn service_failure(error: ServiceError) -> ToolOutcome {
    match error {
        ServiceError::Denied(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
        ServiceError::Cancelled => ToolOutcome::Interrupted,
        other => ToolOutcome::Err(ToolError::message(other.to_string())),
    }
}

fn build_tool(
    runtime: &Runtime,
    name: &str,
    description: &str,
    schema: &str,
) -> Result<Arc<dyn Tool>, RegistrationError> {
    let name =
        Name::parse(name).map_err(|_| RegistrationError::InvalidName { name: name.into() })?;
    let parameters = RawValue::parse(schema).map_err(|_| RegistrationError::InvalidParameters)?;
    if !dal_core::valid_tool_parameters(&parameters) {
        return Err(RegistrationError::InvalidParameters);
    }
    let spec = Arc::new(ToolSpec {
        name: name.clone(),
        description: description.into(),
        parameters,
        grammar: None,
    });
    Ok(Arc::new(OrchestrationTool {
        name,
        spec,
        runtime: runtime.clone(),
    }))
}

fn add_tool(
    builder: ExtensionBuilder,
    runtime: &Runtime,
    name: &str,
    description: &str,
    schema: &str,
) -> Result<ExtensionBuilder, RegistrationError> {
    Ok(builder.tool(
        build_tool(runtime, name, description, schema)?,
        Visibility::Model,
    ))
}

pub(crate) fn report_tool(
    runtime: &Runtime,
) -> Result<(Arc<dyn Tool>, Visibility), RegistrationError> {
    Ok((
        build_tool(runtime, "report", REPORT_DESCRIPTION, REPORT_SCHEMA)?,
        Visibility::Model,
    ))
}

pub(crate) fn register(
    mut builder: ExtensionBuilder,
    runtime: &Runtime,
) -> Result<ExtensionBuilder, RegistrationError> {
    let config = runtime.config();
    if config.agents.enabled {
        builder = add_tool(
            builder,
            runtime,
            "agents",
            AGENTS_DESCRIPTION,
            AGENTS_SCHEMA,
        )?;
    }
    if config.monitor.enabled {
        builder = add_tool(
            builder,
            runtime,
            "monitor",
            MONITOR_DESCRIPTION,
            MONITOR_SCHEMA,
        )?;
    }
    if config.goal.enabled {
        builder = add_tool(
            builder,
            runtime,
            "create_goal",
            CREATE_GOAL_DESCRIPTION,
            CREATE_GOAL_SCHEMA,
        )?;
        builder = add_tool(
            builder,
            runtime,
            "update_goal",
            UPDATE_GOAL_DESCRIPTION,
            UPDATE_GOAL_SCHEMA,
        )?;
        builder = add_tool(
            builder,
            runtime,
            "get_goal",
            GET_GOAL_DESCRIPTION,
            GET_GOAL_SCHEMA,
        )?;
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    use super::{AGENTS_DESCRIPTION, AGENTS_SCHEMA, Runtime, ToolClass, Workspace, build_tool};
    use dal_agent::ext::RawValue;

    #[test]
    fn the_registered_agents_tool_mints_the_git_run_grant() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = crate::orchestration::parse_config(None)?;
        let data = std::env::temp_dir().join("orchestration-data");
        config.data_root = Some(data.clone());
        let runtime = Runtime::new(config)?;
        let tool = build_tool(&runtime, "agents", AGENTS_DESCRIPTION, AGENTS_SCHEMA)?;
        let workspace = Workspace::new(std::env::temp_dir())?;
        let args = RawValue::parse(
            r#"{"action":"run","steps":[{"name":"write","prompt":"write","tools":["patch"],"isolation":"worktree"}]}"#,
        )?;
        let class = tool.classify(&args, &workspace)?;
        let ToolClass::Exec {
            read_only,
            grant: Some(grant),
        } = class
        else {
            return Err("the agents tool must classify a write run with a grant".into());
        };
        assert!(!read_only);
        assert_eq!(grant.argv_prefix.as_ref(), "git");
        assert_eq!(
            grant.roots,
            vec![
                workspace.as_path().to_path_buf(),
                data.join("worktrees"),
                data.join("isolation"),
            ]
        );
        Ok(())
    }
}
