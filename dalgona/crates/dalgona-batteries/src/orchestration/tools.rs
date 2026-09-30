// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::sync::Arc;

use dal_agent::error::{ServiceError, ToolError};
use dal_agent::ext::{
    ArgError, BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
    ToolOutput,
};
use dal_core::{ModelInfo, Name, RegistrationError, ToolClass, ToolSpec, Visibility, Workspace};
use sonic_rs::JsonContainerTrait;

use super::agents_tool::{
    AGENTS_DESCRIPTION, AGENTS_SCHEMA, REPORT_DESCRIPTION, REPORT_SCHEMA,
};
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

    fn classify(&self, args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        if self.name.as_str() != "agents" {
            return Ok(ToolClass::Other);
        }
        let parsed: sonic_rs::Value = sonic_rs::from_str(args.as_str())
            .map_err(|error| ArgError::message(error.to_string()))?;
        let action = parsed
            .as_object().and_then(|object| object.get(&"action"))
            .and_then(sonic_rs::JsonValueTrait::as_str)
            .unwrap_or_default();
        Ok(if action == "run" {
            ToolClass::Exec {
                read_only: false,
                grant: None,
            }
        } else {
            ToolClass::Read
        })
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            match self
                .runtime
                .tool(
                    cx.session(),
                    call.id,
                    self.name.as_str(),
                    call.args,
                    cx.cancel().clone(),
                )
                .await
            {
                Ok(text) => ToolOutcome::Ok(ToolOutput::from_text(text.into_boxed_str())),
                Err(error) => service_failure(error),
            }
        })
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
    let name = Name::parse(name).map_err(|_| RegistrationError::InvalidName { name: name.into() })?;
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

pub(crate) fn report_tool(runtime: &Runtime) -> Result<(Arc<dyn Tool>, Visibility), RegistrationError> {
    Ok((build_tool(runtime, "report", REPORT_DESCRIPTION, REPORT_SCHEMA)?, Visibility::Model))
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
