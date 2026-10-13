//! Model, thinking, mode, and credential commands over cached provider data.
//!
//! Every value here comes from the host-minted [`CommandCx`]; nothing fetches
//! over the network. `/model` resolves against the cached catalog rows only;
//! config aliases fall to `Unknown` until a config read seam lands.

use dal_agent::ext::command::{CommandCx, SaveError};
use dal_core::command::{Chooser, Command, ErrorTriple, FrontAction, Output, Reply};
use dal_core::{Family, Mode, ModelRoute, ThinkingLevel};
use std::fmt::Write as _;

/// The three provider ids dal signs in to.
const PROVIDERS: [&str; 3] = ["anthropic", "openai", "openai-codex"];

/// The thinking levels `/thinking` accepts, in display order.
const THINKING_NAMES: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Opens the settings picker; each row change submits its own command.
pub(super) fn settings() -> Reply {
    Reply::Choose {
        chooser: Chooser::Settings,
        filter: "".into(),
    }
}

/// Runs `/model`: opens the picker or resolves one argument.
pub(super) async fn model(cx: &CommandCx<'_>, arg: Option<&str>) -> Result<Reply, ErrorTriple> {
    let Some(word) = arg else {
        return Ok(Reply::Choose {
            chooser: Chooser::Model,
            filter: "".into(),
        });
    };
    let catalog = cx.catalog();
    let pairs: Vec<(&str, &str)> = catalog
        .as_ref()
        .map(|view| {
            view.entries
                .iter()
                .map(|entry| (&*entry.provider as &str, &*entry.id as &str))
                .collect()
        })
        .unwrap_or_default();
    match resolve_model(&pairs, word) {
        ModelResolution::Mode(mode) => apply_mode(cx, mode).await,
        ModelResolution::Ambiguous { first, second } => Err(ambiguous_model(word, &first, &second)),
        ModelResolution::Route {
            provider,
            id,
            route,
        } => apply_route(cx, &provider, &id, route).await,
        ModelResolution::Unknown => {
            // Config aliases (and nothing else) resolve through the host,
            // which consults the cached catalog plus aliases without fetching.
            match cx.resolve_model(word) {
                Ok(ModelRoute::Harness { id }) => match id.split_once('/') {
                    Some((_, mode)) => match parse_mode(mode) {
                        Some(mode) => apply_mode(cx, mode).await,
                        None => Err(unknown_model(word)),
                    },
                    None => Err(unknown_model(word)),
                },
                Ok(route) => {
                    let (provider, id) = route_origin(&route);
                    apply_route(cx, &provider, &id, route).await
                }
                Err(_) => Err(unknown_model(word)),
            }
        }
    }
}

/// The unknown-model pair for unresolvable `/model` arguments.
pub(super) fn unknown_model(word: &str) -> ErrorTriple {
    super::error_triple(
        format!("Unknown model \"{word}\""),
        "it is not a cached provider id, an alias, or a dalgon mode",
        "Type /model to pick from the list.",
    )
}

/// The ambiguous-model pair for a bare id listed by two providers.
pub(super) fn ambiguous_model(word: &str, first: &str, second: &str) -> ErrorTriple {
    super::error_triple(
        format!("Model id \"{word}\" is ambiguous"),
        format!("it matches {first} and {second}"),
        format!("Type /model {first}/{word} or /model {second}/{word}."),
    )
}

/// The unknown-thinking-level pair, with a suggestion at distance one.
pub(super) fn unknown_thinking(word: &str) -> ErrorTriple {
    let fix = match super::suggest_in(&THINKING_NAMES, word) {
        Some(close) => {
            format!("Did you mean {close}? Type /thinking to pick from the list.")
        }
        None => "Type /thinking to pick from the list.".to_owned(),
    };
    super::error_triple(
        format!("Unknown thinking level \"{word}\""),
        "the levels are off, minimal, low, medium, high, xhigh, max",
        fix,
    )
}

/// The unsupported-level pair naming the levels the saved route supports.
pub(super) fn unsupported_thinking(level: &str, whose: &str, levels: &str) -> ErrorTriple {
    super::error_triple(
        format!("Thinking level \"{level}\" is not available"),
        format!("{whose} supports {levels}"),
        "Type /thinking with one of those levels.",
    )
}

/// The no-cached-catalog pair for `/scoped-models` without a model list.
pub(super) fn no_catalog() -> ErrorTriple {
    super::error_triple(
        "No model list is cached",
        "dalgon has not fetched one yet",
        "Type /model to fetch the list, then try again.",
    )
}

/// The unknown-mode pair for `/mode` arguments outside the three modes.
pub(super) fn unknown_mode(arg: &str) -> ErrorTriple {
    super::error_triple(
        format!("Unknown mode \"{arg}\""),
        "the modes are normal, eval-first, eval-only",
        "Type /mode normal, /mode eval-first, or /mode eval-only.",
    )
}

/// The unknown-provider pair for `/login` and `/logout`.
pub(super) fn unknown_provider(cmd: &str, provider: &str) -> ErrorTriple {
    let fix = match super::suggest_in(&PROVIDERS, provider) {
        Some(close) => {
            format!("Did you mean {close}? Type /{cmd} to pick from the list.")
        }
        None => format!("Type /{cmd} to pick from the list."),
    };
    super::error_triple(
        format!("Unknown provider \"{provider}\""),
        "dalgon signs in to anthropic, openai, and openai-codex",
        fix,
    )
}

/// Submits a harness mode and prints the mode line, keeping the model.
async fn apply_mode(cx: &CommandCx<'_>, mode: Mode) -> Result<Reply, ErrorTriple> {
    let _ = cx
        .submit_wait(Command::SetMode {
            mode,
            save: dal_core::command::Save::SessionAndDefault,
        })
        .await;
    let line = match cx.view().settings.model.as_ref() {
        Some(route) => {
            let (provider, id) = route_origin(route);
            format!(
                "Mode: {}. The model stays {provider}/{id}. Saved as the default.",
                mode.as_str()
            )
        }
        None => format!("Mode: {}. Saved as the default.", mode.as_str()),
    };
    Ok(Reply::Done(Output::Text(line.into())))
}

/// Submits a model route and prints the model line with its save outcome.
async fn apply_route(
    cx: &CommandCx<'_>,
    provider: &str,
    id: &str,
    route: ModelRoute,
) -> Result<Reply, ErrorTriple> {
    if !cx
        .auth_stored()
        .iter()
        .any(|(stored, _)| &**stored == provider)
    {
        return Err(super::error_triple(
            format!("{provider} has no credentials"),
            "auth.json has no entry for it",
            format!("Type /login {provider} to sign in, then try again."),
        ));
    }
    let old = cx.view().settings.model.clone();
    let after = cx.edit_style_for(&route);
    let saved = match cx
        .submit_wait(Command::SetModel {
            model: route,
            save: dal_core::command::Save::SessionAndDefault,
        })
        .await
    {
        Ok(()) => None,
        Err(SaveError::Failed { message }) => Some(message.to_string()),
        Err(SaveError::SessionClosed { id }) => Some(format!("session {id} is closed")),
        Err(_) => Some("the save failed".to_owned()),
    };
    let mut line = format!("Model: {provider}/{id}.");
    if cx.turn().is_some() {
        line.push_str(" It applies from the next request.");
    }
    match saved {
        None => line.push_str(" Saved as the default."),
        Some(reason) => {
            let _ = write!(line, " It was not saved as the default: {reason}.");
        }
    }
    if let Some(previous) = old {
        let before = cx.edit_style_for(&previous);
        if before != after {
            let _ = write!(line, "\nEdit style: {after}. It follows the model.");
        }
    }
    Ok(Reply::Done(Output::Text(line.into())))
}

/// Renders the saved route as the `provider/id` the model lines print.
fn route_origin(route: &ModelRoute) -> (String, String) {
    match route {
        ModelRoute::Api { family, model } => {
            let provider = match family {
                Family::Anthropic => "anthropic",
                Family::Codex => "openai-codex",
                Family::Responses => "openai",
                Family::Chat => "openai-chat",
            };
            (provider.to_owned(), model.to_string())
        }
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => match id.split_once('/') {
            Some((provider, rest)) => (provider.to_owned(), rest.to_owned()),
            None => ("dalgon".to_owned(), id.to_string()),
        },
    }
}

/// One `/model` argument resolved against cached `(provider, id)` rows.
#[derive(Debug)]
pub(super) enum ModelResolution {
    /// A harness mode id: submit `SetMode`, keep the model.
    Mode(Mode),
    /// No row, mode, or host alias matched.
    Unknown,
    /// A bare id listed by two providers.
    Ambiguous { first: Box<str>, second: Box<str> },
    /// A catalog row ready to submit.
    Route {
        provider: Box<str>,
        id: Box<str>,
        route: ModelRoute,
    },
}

/// Maps a provider id to the API family its models route through.
fn provider_family(provider: &str) -> Family {
    match provider {
        "anthropic" => Family::Anthropic,
        "openai-codex" => Family::Codex,
        "openai" => Family::Responses,
        _ => Family::Chat,
    }
}

/// Resolves a `/model` argument against cached `(provider, id)` rows.
///
/// Mode ids win first; an exact `provider/id` row wins next; a bare id
/// needs exactly one row. Anything else falls to `Unknown` here and
/// resolves through the host (config aliases) in the caller.
pub(super) fn resolve_model(pairs: &[(&str, &str)], arg: &str) -> ModelResolution {
    if let Some(mode) = parse_mode(arg) {
        return ModelResolution::Mode(mode);
    }
    if let Some(rest) = arg.strip_prefix("dalgon/") {
        if let Some(mode) = parse_mode(rest) {
            return ModelResolution::Mode(mode);
        }
        return ModelResolution::Unknown;
    }
    if let Some((provider, id)) = arg.split_once('/') {
        if let Some((p, i)) = pairs.iter().find(|(p, i)| *p == provider && *i == id) {
            return ModelResolution::Route {
                provider: (*p).into(),
                id: (*i).into(),
                route: ModelRoute::Api {
                    family: provider_family(p),
                    model: (*i).into(),
                },
            };
        }
        // Anything else (aliases, typed ids) resolves through the host,
        // which consults the cached catalog plus config aliases; an
        // unknown synthetic id fails there with the model errors below.
        return ModelResolution::Unknown;
    }
    let mut matches = pairs.iter().filter(|(_, i)| *i == arg);
    match (matches.next(), matches.next()) {
        (None, _) => ModelResolution::Unknown,
        (Some((p, i)), None) => ModelResolution::Route {
            provider: (*p).into(),
            id: (*i).into(),
            route: ModelRoute::Api {
                family: provider_family(p),
                model: (*i).into(),
            },
        },
        (Some((first, _)), Some((second, _))) => ModelResolution::Ambiguous {
            first: (*first).into(),
            second: (*second).into(),
        },
    }
}

/// Parses a harness mode from its bare or `dalgon/`-qualified spelling.
pub(super) fn parse_mode(word: &str) -> Option<Mode> {
    match word {
        "normal" => Some(Mode::Normal),
        "eval-first" => Some(Mode::EvalFirst),
        "eval-only" => Some(Mode::EvalOnly),
        _ => None,
    }
}

/// Runs `/thinking`: opens the picker or sets one level.
pub(super) async fn thinking(cx: &CommandCx<'_>, arg: Option<&str>) -> Result<Reply, ErrorTriple> {
    let Some(word) = arg else {
        return Ok(Reply::Choose {
            chooser: Chooser::Thinking,
            filter: "".into(),
        });
    };
    let Some(level) = parse_thinking(word) else {
        return Err(unknown_thinking(word));
    };
    let supported = cx.levels_for();
    if !supported.contains(&level) {
        let whose =
            cx.view()
                .settings
                .model
                .as_ref()
                .map_or("no saved model".to_owned(), |route| {
                    let (provider, id) = route_origin(route);
                    format!("{provider}/{id}")
                });
        let levels = supported
            .iter()
            .map(ThinkingLevel::name)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(unsupported_thinking(level.name(), &whose, &levels));
    }
    let _ = cx
        .submit_wait(Command::SetThinking {
            level,
            save: dal_core::command::Save::SessionAndDefault,
        })
        .await;
    Ok(Reply::Done(Output::Text(
        format!("Thinking: {}. Saved as the default.", level.name()).into(),
    )))
}

/// Parses a thinking level from its canonical spelling.
pub(super) fn parse_thinking(word: &str) -> Option<ThinkingLevel> {
    match word {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

/// Runs `/scoped-models`: opens the multi-select over the cached catalog.
pub(super) fn scoped_models(cx: &CommandCx<'_>) -> Result<Reply, ErrorTriple> {
    if cx.catalog().is_none() {
        return Err(no_catalog());
    }
    Ok(Reply::Choose {
        chooser: Chooser::ScopedModels,
        filter: "".into(),
    })
}

/// Runs `/mode`: maps one word to `SetMode`, untouched on anything else.
pub(super) async fn mode(cx: &CommandCx<'_>, arg: &str) -> Result<Reply, ErrorTriple> {
    let Some(mode) = parse_mode(arg) else {
        return Err(unknown_mode(arg));
    };
    let _ = cx
        .submit_wait(Command::SetMode {
            mode,
            save: dal_core::command::Save::SessionAndDefault,
        })
        .await;
    Ok(Reply::Done(Output::Text(
        format!("Mode: {}. Saved as the default.", mode.as_str()).into(),
    )))
}

/// Runs `/login`.
pub(super) fn login(arg: Option<&str>) -> Result<Reply, ErrorTriple> {
    login_or_logout("login", Chooser::Login, arg)
}

/// Runs `/logout`.
pub(super) fn logout(arg: Option<&str>) -> Result<Reply, ErrorTriple> {
    login_or_logout("logout", Chooser::Logout, arg)
}

fn login_or_logout(cmd: &str, chooser: Chooser, arg: Option<&str>) -> Result<Reply, ErrorTriple> {
    let Some(provider) = arg else {
        return Ok(Reply::Choose {
            chooser,
            filter: "".into(),
        });
    };
    if !PROVIDERS.contains(&provider) {
        return Err(unknown_provider(cmd, provider));
    }
    let provider: Box<str> = provider.into();
    if cmd == "login" {
        Ok(Reply::Front(FrontAction::Login { provider }))
    } else {
        Ok(Reply::Front(FrontAction::Logout { provider }))
    }
}
