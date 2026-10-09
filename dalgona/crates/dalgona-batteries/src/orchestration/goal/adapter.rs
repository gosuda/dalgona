// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use dal_agent::error::ServiceError;
use dal_agent::ext::{Caller, Services};
use dal_core::{SidecarName, SidecarOp, Timestamp};
use serde::Deserialize;

use super::super::ControllerMode;
use super::super::monitor::status::InflightCounts;
use super::ops::{
    GoalCommand, GoalScope, TodoSummary, UpdateTarget, apply_goal_command, create_goal, get_goal,
    parse_goal_command, update_goal,
};
use super::sidecar::{GoalError, GoalSidecar, controller_wire, decode_sidecar, encode_sidecar};

#[derive(Clone, Debug)]
pub(crate) struct GoalStore {
    pub sidecar: Option<GoalSidecar>,
    pub saved: bool,
    pub error: Option<GoalError>,
}

impl GoalStore {
    pub(crate) fn empty(session: &str, mode: ControllerMode) -> Self {
        Self {
            sidecar: Some(GoalSidecar {
                v: 1,
                session: session.into(),
                controller: mode,
                next_goal: 1,
                goal: None,
            }),
            saved: false,
            error: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    objective: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    status: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetArgs {}

pub(crate) async fn load(services: &dyn Services, caller: &Caller, session: &str) -> GoalStore {
    let Ok(name) = SidecarName::parse("goal.json") else {
        return failed_store(GoalError::StoreUnavailable {
            message: "the goal sidecar name is invalid".into(),
        });
    };
    match services.sidecar(caller, SidecarOp::Read { name }).await {
        Ok(None) => GoalStore {
            saved: true,
            ..GoalStore::empty(
                session,
                ControllerMode::Paused {
                    reason: "session opened",
                },
            )
        },
        Ok(Some(bytes)) => match decode_sidecar(&bytes, session) {
            Ok(sidecar) => GoalStore {
                sidecar: Some(sidecar),
                saved: true,
                error: None,
            },
            Err(error) => GoalStore {
                sidecar: None,
                saved: true,
                error: Some(error),
            },
        },
        Err(error) => failed_store(GoalError::StoreUnavailable {
            message: error.to_string().into_boxed_str(),
        }),
    }
}

fn failed_store(error: GoalError) -> GoalStore {
    GoalStore {
        sidecar: None,
        saved: false,
        error: Some(error),
    }
}

pub(crate) struct ToolContext<'a, 'b> {
    pub(crate) store: &'a mut GoalStore,
    pub(crate) ctx: &'a GoalScope<'b>,
    pub(crate) todos: &'a TodoSummary,
    pub(crate) inflight: &'a InflightCounts,
    pub(crate) services: &'a dyn Services,
    pub(crate) caller: &'a Caller,
}

pub(crate) async fn tool(
    name: &str,
    args: &str,
    context: ToolContext<'_, '_>,
) -> Result<String, ServiceError> {
    let Some(sidecar) = context.store.sidecar.as_mut() else {
        return Err(service_failure(context.store.error.as_ref()));
    };
    let now = Timestamp::now();
    let reply = match name {
        "create_goal" => {
            let input = decode::<CreateArgs>(args, name)?;
            create_goal(sidecar, context.ctx, &input.objective, now)
                .map_err(|error| goal_failure(&error))?
        }
        "update_goal" => {
            let input = decode::<UpdateArgs>(args, name)?;
            let target = match input.status.as_str() {
                "complete" => UpdateTarget::Complete,
                "blocked" => UpdateTarget::Blocked,
                _ => {
                    return Err(ServiceError::failed(
                        None,
                        "update_goal: status must be complete or blocked.",
                    ));
                }
            };
            update_goal(
                sidecar,
                context.ctx,
                target,
                input.reason.as_deref(),
                context.todos,
                context.inflight,
                now,
            )
            .map_err(|error| goal_failure(&error))?
        }
        "get_goal" => {
            let _ = decode::<GetArgs>(args, name)?;
            return get_goal(sidecar, context.ctx).map_err(|error| goal_failure(&error));
        }
        _ => {
            return Err(ServiceError::failed(
                None,
                "unknown orchestration goal tool",
            ));
        }
    };
    save(context.services, context.caller, sidecar).await?;
    Ok(reply)
}

pub(crate) async fn command(
    args: &str,
    store: &mut GoalStore,
    ctx: &GoalScope<'_>,
    services: &dyn Services,
    caller: &Caller,
) -> Result<String, ServiceError> {
    if let Some(error) = store.error.as_ref() {
        return Err(goal_failure(error));
    }
    let Some(sidecar) = store.sidecar.as_mut() else {
        return Err(ServiceError::failed(
            None,
            "goal: the goal sidecar is unavailable.",
        ));
    };
    let action = parse_goal_command(args);
    let reply = apply_goal_command(sidecar, ctx, &action, Timestamp::now());
    if action != GoalCommand::Show {
        save(services, caller, sidecar).await?;
    }
    Ok(reply)
}

pub(crate) async fn save(
    services: &dyn Services,
    caller: &Caller,
    sidecar: &GoalSidecar,
) -> Result<(), ServiceError> {
    let name =
        SidecarName::parse("goal.json").map_err(|_| ServiceError::sidecar_bad_name("goal.json"))?;
    let bytes = encode_sidecar(sidecar).map_err(|error| goal_failure(&error))?;
    services
        .sidecar(caller, SidecarOp::Write { name, bytes })
        .await
        .map_err(|error| {
            goal_failure(&GoalError::SaveFailed {
                message: error.to_string().into(),
            })
        })?;
    Ok(())
}

pub(crate) fn update_mode(store: &mut GoalStore, mode: ControllerMode) {
    let Some(sidecar) = store.sidecar.as_mut() else {
        return;
    };
    sidecar.controller = mode;
}

pub(crate) fn persisted_mode(store: &GoalStore) -> Option<&'static str> {
    store
        .sidecar
        .as_ref()
        .map(|sidecar| controller_wire(sidecar.controller))
}

fn decode<T: for<'de> Deserialize<'de>>(args: &str, tool: &str) -> Result<T, ServiceError> {
    sonic_rs::from_str(args)
        .map_err(|error| ServiceError::failed(None, format!("{tool}: invalid arguments: {error}")))
}

fn goal_failure(error: &GoalError) -> ServiceError {
    ServiceError::failed(None, error.to_string())
}

fn service_failure(error: Option<&GoalError>) -> ServiceError {
    error.map_or_else(
        || ServiceError::failed(None, "goal: the goal sidecar is unavailable."),
        |error| ServiceError::failed(None, error.to_string()),
    )
}
