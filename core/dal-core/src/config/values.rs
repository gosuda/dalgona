use super::parse::{RULES_KEYS, SERVE_KEYS, unknown_key};
use super::partial::{
    PartialGuard, PartialGuardBands, PartialGuardPolicies, PartialRules, PartialServe, RulesText,
    ServeText,
};
use super::prices::{parse_rules_choice, parse_rules_disabled, parse_rules_integer, rules_invalid};
use super::{
    ApprovalMode, ConfigError, ConfigProduct, EditStyleInput, GuardCheckMode, JudgeMode, Mode,
    PathBuf, Screen, invalid_value, value_text,
};

pub(super) fn parse_mode(value: toml::Value) -> Result<Mode, ConfigError> {
    const EXPECTED: &str = "normal, eval-first, eval-only";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "normal" => Ok(Mode::Normal),
            "eval-first" => Ok(Mode::EvalFirst),
            "eval-only" => Ok(Mode::EvalOnly),
            _ => Err(invalid_value("mode", value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value("mode", value_text(&value), EXPECTED)),
    }
}

pub(super) fn parse_thinking(
    value: toml::Value,
) -> Result<crate::model::ThinkingLevel, ConfigError> {
    use crate::model::ThinkingLevel;
    const EXPECTED: &str = "off, minimal, low, medium, high, xhigh, max";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "off" => Ok(ThinkingLevel::Off),
            "minimal" => Ok(ThinkingLevel::Minimal),
            "low" => Ok(ThinkingLevel::Low),
            "medium" => Ok(ThinkingLevel::Medium),
            "high" => Ok(ThinkingLevel::High),
            "xhigh" => Ok(ThinkingLevel::Xhigh),
            "max" => Ok(ThinkingLevel::Max),
            _ => Err(invalid_value("thinking", value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value("thinking", value_text(&value), EXPECTED)),
    }
}

pub(super) fn parse_approval(key: &str, value: toml::Value) -> Result<ApprovalMode, ConfigError> {
    const EXPECTED: &str = "ask, edits, all";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "ask" => Ok(ApprovalMode::Ask),
            "edits" => Ok(ApprovalMode::Edits),
            "all" => Ok(ApprovalMode::All),
            _ => Err(invalid_value(key, value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value(key, value_text(&value), EXPECTED)),
    }
}

pub(super) fn parse_screen(value: toml::Value) -> Result<Screen, ConfigError> {
    const EXPECTED: &str = "inline, fullscreen";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "inline" => Ok(Screen::Inline),
            "fullscreen" => Ok(Screen::Fullscreen),
            _ => Err(invalid_value("screen", value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value("screen", value_text(&value), EXPECTED)),
    }
}

pub(super) fn parse_edit_style(value: toml::Value) -> Result<EditStyleInput, ConfigError> {
    match value {
        toml::Value::String(style) => Ok(EditStyleInput::Scalar(style.into_boxed_str())),
        toml::Value::Table(table) => {
            let mut rows = Vec::with_capacity(table.len());
            for (key, style) in table {
                let style = match style {
                    toml::Value::String(style) => style.into_boxed_str(),
                    other => {
                        return Err(invalid_value(
                            "edit_style",
                            value_text(&other),
                            "string style name",
                        ));
                    }
                };
                rows.push((key.into_boxed_str(), style));
            }
            Ok(EditStyleInput::Table(rows))
        }
        other => Err(invalid_value(
            "edit_style",
            value_text(&other),
            "string or table",
        )),
    }
}

pub(super) fn parse_nonempty_string(
    key: &str,
    value: toml::Value,
    expected: &str,
) -> Result<Box<str>, ConfigError> {
    match value {
        toml::Value::String(value) if !value.is_empty() => Ok(value.into_boxed_str()),
        toml::Value::String(value) => Err(invalid_value(key, value.into_boxed_str(), expected)),
        value => Err(invalid_value(key, value_text(&value), expected)),
    }
}

pub(super) fn parse_bool(key: &str, value: toml::Value) -> Result<bool, ConfigError> {
    match value {
        toml::Value::Boolean(value) => Ok(value),
        value => Err(invalid_value(key, value_text(&value), "boolean")),
    }
}
pub(super) fn parse_guard(value: toml::Value) -> Result<PartialGuard, ConfigError> {
    match value {
        toml::Value::Table(table) => {
            for key in table.keys() {
                if key != "enabled" && key != "policies" {
                    return Err(ConfigError::UnknownGuardKey {
                        key: Box::<str>::from(key.as_str()),
                    });
                }
            }
            let enabled = match table.get("enabled") {
                None => None,
                Some(toml::Value::Boolean(enabled)) => Some(*enabled),
                Some(other) => {
                    return Err(invalid_value("guard.enabled", value_text(other), "boolean"));
                }
            };
            let policies = match table.get("policies") {
                None => None,
                Some(toml::Value::Table(policies)) => Some(parse_guard_policies(policies)?),
                Some(other) => {
                    return Err(invalid_value("guard.policies", value_text(other), "table"));
                }
            };
            Ok(PartialGuard { enabled, policies })
        }
        other => Err(invalid_value("guard", value_text(&other), "table")),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per [guard.policies] key keeps the closed key list readable"
)]
pub(super) fn parse_guard_policies(
    table: &toml::map::Map<String, toml::Value>,
) -> Result<PartialGuardPolicies, ConfigError> {
    for key in table.keys() {
        match key.as_str() {
            "bands"
            | "g2_mode"
            | "g3_mode"
            | "g4_enabled"
            | "g8_calibrated_rules"
            | "erosion_report_threshold"
            | "churn_turn_threshold" => {}
            other => {
                return Err(ConfigError::UnknownGuardKey {
                    key: Box::<str>::from(other),
                });
            }
        }
    }
    let bands = match table.get("bands") {
        None => None,
        Some(toml::Value::Table(bands)) => Some(parse_guard_bands(bands)?),
        Some(other) => {
            return Err(invalid_value(
                "guard.policies.bands",
                value_text(other),
                "table",
            ));
        }
    };
    let g2_mode = match table.get("g2_mode") {
        None => None,
        Some(value) => Some(parse_guard_mode("guard.policies.g2_mode", value)?),
    };
    let g3_mode = match table.get("g3_mode") {
        None => None,
        Some(value) => Some(parse_guard_mode("guard.policies.g3_mode", value)?),
    };
    let g4_enabled = match table.get("g4_enabled") {
        None => None,
        Some(toml::Value::Boolean(enabled)) => Some(*enabled),
        Some(other) => {
            return Err(invalid_value(
                "guard.policies.g4_enabled",
                value_text(other),
                "boolean",
            ));
        }
    };
    let g8_calibrated_rules = match table.get("g8_calibrated_rules") {
        None => None,
        Some(toml::Value::Array(items)) => {
            let mut rules = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    toml::Value::String(rule) => rules.push(rule.clone()),
                    other => {
                        return Err(invalid_value(
                            "guard.policies.g8_calibrated_rules",
                            value_text(other),
                            "array of strings",
                        ));
                    }
                }
            }
            Some(rules)
        }
        Some(other) => {
            return Err(invalid_value(
                "guard.policies.g8_calibrated_rules",
                value_text(other),
                "array of strings",
            ));
        }
    };
    let erosion_report_threshold = match table.get("erosion_report_threshold") {
        None => None,
        Some(toml::Value::Integer(value)) => {
            let threshold = f64::from(i32::try_from(*value).map_err(|_| {
                invalid_value(
                    "guard.policies.erosion_report_threshold",
                    format!("{value}"),
                    "finite number",
                )
            })?);
            Some(threshold)
        }
        Some(toml::Value::Float(threshold)) => {
            if !threshold.is_finite() {
                return Err(invalid_value(
                    "guard.policies.erosion_report_threshold",
                    format!("{threshold}"),
                    "finite number",
                ));
            }
            Some(*threshold)
        }
        Some(other) => {
            return Err(invalid_value(
                "guard.policies.erosion_report_threshold",
                value_text(other),
                "finite number",
            ));
        }
    };
    let churn_turn_threshold = match table.get("churn_turn_threshold") {
        None => None,
        Some(toml::Value::Integer(threshold)) => Some(u32::try_from(*threshold).map_err(|_| {
            invalid_value(
                "guard.policies.churn_turn_threshold",
                format!("{threshold}"),
                "non-negative integer",
            )
        })?),
        Some(other) => {
            return Err(invalid_value(
                "guard.policies.churn_turn_threshold",
                value_text(other),
                "non-negative integer",
            ));
        }
    };
    Ok(PartialGuardPolicies {
        bands,
        g2_mode,
        g3_mode,
        g4_enabled,
        g8_calibrated_rules,
        erosion_report_threshold,
        churn_turn_threshold,
    })
}

pub(super) fn parse_guard_bands(
    table: &toml::map::Map<String, toml::Value>,
) -> Result<PartialGuardBands, ConfigError> {
    for key in table.keys() {
        match key.as_str() {
            "cognitive" | "cyclomatic" | "function_ploc" | "nesting" | "file_ploc" => {}
            other => {
                return Err(ConfigError::UnknownGuardKey {
                    key: Box::<str>::from(other),
                });
            }
        }
    }
    let band = |key: &str| -> Result<Option<u32>, ConfigError> {
        match table.get(key) {
            None => Ok(None),
            Some(toml::Value::Integer(value)) => Ok(Some(u32::try_from(*value).map_err(|_| {
                invalid_value(
                    &format!("guard.policies.bands.{key}"),
                    format!("{value}"),
                    "non-negative integer",
                )
            })?)),
            Some(other) => Err(invalid_value(
                &format!("guard.policies.bands.{key}"),
                value_text(other),
                "non-negative integer",
            )),
        }
    };
    Ok(PartialGuardBands {
        cognitive: band("cognitive")?,
        cyclomatic: band("cyclomatic")?,
        function_ploc: band("function_ploc")?,
        nesting: band("nesting")?,
        file_ploc: band("file_ploc")?,
    })
}

pub(super) fn parse_guard_mode(
    key: &str,
    value: &toml::Value,
) -> Result<GuardCheckMode, ConfigError> {
    match value {
        toml::Value::String(mode) => match mode.as_str() {
            "report" => Ok(GuardCheckMode::Report),
            "block_after_calibration" => Ok(GuardCheckMode::BlockAfterCalibration),
            _ => Err(invalid_value(
                key,
                Box::<str>::from(mode.as_str()),
                "report, block_after_calibration",
            )),
        },
        other => Err(invalid_value(
            key,
            value_text(other),
            "report, block_after_calibration",
        )),
    }
}

pub(super) fn parse_port(value: toml::Value) -> Result<u16, ConfigError> {
    match value {
        toml::Value::Integer(value) => u16::try_from(value)
            .map_err(|_| invalid_value("serve.port", format!("{value}"), "integer 0..=65535")),
        value => Err(invalid_value(
            "serve.port",
            value_text(&value),
            "integer 0..=65535",
        )),
    }
}

pub(super) fn parse_number(
    key: &str,
    value: toml::Value,
    expected: &str,
) -> Result<f64, ConfigError> {
    match value {
        toml::Value::Integer(value) => toml::Value::Integer(value)
            .try_into()
            .map_err(|_| invalid_value(key, format!("{value}"), expected)),
        toml::Value::Float(value) => Ok(value),
        value => Err(invalid_value(key, value_text(&value), expected)),
    }
}

pub(super) fn parse_strings(
    key: &str,
    value: toml::Value,
    expected: &str,
) -> Result<Vec<Box<str>>, ConfigError> {
    let values = match value {
        toml::Value::Array(values) => values,
        other => return Err(invalid_value(key, value_text(&other), expected)),
    };
    values
        .into_iter()
        .map(|value| match value {
            toml::Value::String(value) => Ok(value.into_boxed_str()),
            value => Err(invalid_value(key, value_text(&value), expected)),
        })
        .collect()
}

pub(super) fn parse_nonempty_strings(
    key: &str,
    value: toml::Value,
    expected: &str,
) -> Result<Vec<Box<str>>, ConfigError> {
    let values = parse_strings(key, value, expected)?;
    if let Some(empty) = values.iter().find(|value| value.is_empty()) {
        return Err(invalid_value(
            key,
            Box::<str>::from(empty.as_ref()),
            expected,
        ));
    }
    Ok(values)
}

pub(super) fn parse_sandbox_writable(
    sections: &std::collections::BTreeMap<Box<str>, toml::Value>,
) -> Result<Vec<Box<str>>, ConfigError> {
    let Some(value) = sections.get("sandbox_writable") else {
        return Ok(Vec::new());
    };
    let toml::Value::Array(values) = value else {
        return Err(invalid_value(
            "sandbox_writable",
            value_text(value),
            "array of path strings",
        ));
    };
    values
        .iter()
        .map(|value| match value {
            toml::Value::String(value) => Ok(Box::<str>::from(value.as_str())),
            value => Err(invalid_value(
                "sandbox_writable",
                value_text(value),
                "array of path strings",
            )),
        })
        .collect()
}

pub(super) fn parse_aliases(
    value: toml::Value,
) -> Result<std::collections::BTreeMap<Box<str>, Box<str>>, ConfigError> {
    let aliases = match value {
        toml::Value::Table(aliases) => aliases,
        other => {
            return Err(invalid_value(
                "aliases",
                value_text(&other),
                "table of model ids",
            ));
        }
    };
    let mut parsed = std::collections::BTreeMap::new();
    for (alias, value) in aliases {
        let key = format!("aliases.{alias}");
        let model = parse_nonempty_string(&key, value, "non-empty model id")?;
        parsed.insert(alias.into_boxed_str(), model);
    }
    Ok(parsed)
}

pub(super) fn parse_path(value: toml::Value) -> Result<PathBuf, ConfigError> {
    match value {
        toml::Value::String(value) => Ok(PathBuf::from(value)),
        value => Err(invalid_value(
            "serve.token_file",
            value_text(&value),
            "path string",
        )),
    }
}

pub(super) fn parse_serve(
    product: ConfigProduct,
    value: toml::Value,
) -> Result<PartialServe, ConfigError> {
    let table = match value {
        toml::Value::Table(table) => table,
        other => return Err(invalid_value("serve", value_text(&other), "table")),
    };
    for key in table.keys() {
        if !SERVE_KEYS.contains(&key.as_str()) {
            return Err(unknown_key(&format!("serve.{key}"), product, None));
        }
    }
    let text: ServeText =
        toml::Value::Table(table)
            .try_into()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: Box::<str>::from(error.message()),
            })?;
    let mut serve = PartialServe::default();
    if let Some(value) = text.bind {
        serve.bind = Some(parse_nonempty_string(
            "serve.bind",
            value,
            "non-empty address string",
        )?);
    }
    if let Some(value) = text.port {
        serve.port = Some(parse_port(value)?);
    }
    if let Some(value) = text.token_file {
        serve.token_file = Some(parse_path(value)?);
    }
    if let Some(value) = text.approval {
        serve.approval = Some(parse_approval("serve.approval", value)?);
    }
    if let Some(value) = text.origins {
        serve.origins = Some(parse_strings(
            "serve.origins",
            value,
            "array of exact origin strings",
        )?);
    }
    Ok(serve)
}

pub(super) fn parse_rules(
    product: ConfigProduct,
    value: toml::Value,
) -> Result<PartialRules, ConfigError> {
    use crate::ext::{InterruptMode, RepeatMode};

    let table = match value {
        toml::Value::Table(table) => table,
        other => return Err(invalid_value("rules", value_text(&other), "table")),
    };
    for key in table.keys() {
        if !RULES_KEYS.contains(&key.as_str()) {
            return Err(unknown_key(&format!("rules.{key}"), product, None));
        }
    }
    let text: RulesText =
        toml::Value::Table(table)
            .try_into()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: Box::<str>::from(error.message()),
            })?;
    let mut rules = PartialRules::default();
    if let Some(value) = text.watch {
        rules.watch = Some(match value {
            toml::Value::Boolean(watch) => watch,
            other => return Err(rules_invalid("rules.watch", &other, "Use true or false.")),
        });
    }
    if let Some(value) = text.interrupt {
        rules.interrupt = Some(parse_rules_choice(
            "rules.interrupt",
            &value,
            &[
                ("always", InterruptMode::Always),
                ("prose-only", InterruptMode::ProseOnly),
                ("tool-only", InterruptMode::ToolOnly),
                ("never", InterruptMode::Never),
            ],
            "Use one of always, prose-only, tool-only, never.",
        )?);
    }
    if let Some(value) = text.repeat {
        rules.repeat = Some(parse_rules_choice(
            "rules.repeat",
            &value,
            &[
                ("once", RepeatMode::Once),
                ("after-gap", RepeatMode::AfterGap),
            ],
            "Use one of once, after-gap.",
        )?);
    }
    if let Some(value) = text.repeat_gap {
        rules.repeat_gap = Some(parse_rules_integer(
            "rules.repeat_gap",
            &value,
            1..=1000,
            "Use a whole number from 1 to 1000.",
        )?);
    }
    if let Some(value) = text.max_retries {
        rules.max_retries = Some(parse_rules_integer(
            "rules.max_retries",
            &value,
            0..=20,
            "Use a whole number from 0 to 20.",
        )?);
    }
    if let Some(value) = text.disabled {
        rules.disabled = Some(parse_rules_disabled(&value)?);
    }
    if let Some(value) = text.judge {
        rules.judge = Some(parse_rules_choice(
            "rules.judge",
            &value,
            &[
                ("auto", JudgeMode::Auto),
                ("on", JudgeMode::On),
                ("off", JudgeMode::Off),
            ],
            "Use one of auto, on, off.",
        )?);
    }
    Ok(rules)
}

/// Wire names accepted for `model_route.family`: the [`crate::model::Family`] serde names.
const MODEL_ROUTE_FAMILIES: &str = "openai_chat, openai_responses, openai_codex, anthropic, bedrock, cursor, devin, droid, gitlab, google-generative-ai, google-vertex, google-gemini-cli, google-antigravity, mistral, ollama, pi-messages";

fn parse_model_route_family(value: &str) -> Result<crate::model::Family, ConfigError> {
    use crate::model::Family;
    match value {
        "openai_chat" => Ok(Family::Chat),
        "openai_responses" => Ok(Family::Responses),
        "openai_codex" => Ok(Family::Codex),
        "anthropic" => Ok(Family::Anthropic),
        "bedrock" => Ok(Family::Bedrock),
        "cursor" => Ok(Family::Cursor),
        "devin" => Ok(Family::Devin),
        "droid" => Ok(Family::Droid),
        "gitlab" => Ok(Family::GitLab),
        "google-generative-ai" => Ok(Family::GoogleGenerativeAi),
        "google-vertex" => Ok(Family::GoogleVertex),
        "google-gemini-cli" => Ok(Family::GoogleGeminiCli),
        "google-antigravity" => Ok(Family::GoogleAntigravity),
        "mistral" => Ok(Family::Mistral),
        "ollama" => Ok(Family::Ollama),
        "pi-messages" => Ok(Family::PiMessages),
        other => Err(invalid_value(
            "model_route.family",
            Box::<str>::from(other),
            MODEL_ROUTE_FAMILIES,
        )),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelRouteText {
    kind: Option<toml::Value>,
    family: Option<toml::Value>,
    model: Option<toml::Value>,
    id: Option<toml::Value>,
    provider: Option<toml::Value>,
}

/// Parses the authoritative structured default route from the top-level
/// `model_route` table.
///
/// `kind = "api"` needs `family` (a [`crate::model::Family`] wire name) plus `model` (a
/// non-empty provider-native id, which may contain `/`, uppercase, `.` or
/// `:`). `kind = "synthetic"` or `"harness"` needs `id`, checked against
/// the closed synthetic or harness grammar. Members that do not belong to
/// the selected kind are rejected so a mistyped table cannot silently drop
/// the family that the legacy `model` string cannot encode.
pub(super) fn parse_model_route(
    value: toml::Value,
) -> Result<crate::model::ModelRoute, ConfigError> {
    const EXPECTED: &str = "table with kind api/synthetic/harness";
    let toml::Value::Table(table) = value else {
        return Err(invalid_value("model_route", value_text(&value), EXPECTED));
    };
    let text: ModelRouteText =
        toml::Value::Table(table)
            .try_into()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: Box::<str>::from(error.message()),
            })?;
    let kind = match &text.kind {
        Some(toml::Value::String(kind)) => kind.clone(),
        Some(other) => {
            return Err(invalid_value(
                "model_route.kind",
                value_text(other),
                "api, synthetic, harness",
            ));
        }
        None => {
            return Err(invalid_value(
                "model_route.kind",
                "missing",
                "api, synthetic, harness",
            ));
        }
    };
    match kind.as_str() {
        "api" => parse_model_route_api(text),
        "synthetic" | "harness" => parse_model_route_id(text, &kind),
        other => Err(invalid_value(
            "model_route.kind",
            Box::<str>::from(other),
            "api, synthetic, harness",
        )),
    }
}

/// Builds the structured `kind = "api"` route after rejecting the members
/// the api kind does not own.
fn parse_model_route_api(text: ModelRouteText) -> Result<crate::model::ModelRoute, ConfigError> {
    if let Some(unexpected) = text.id.as_ref() {
        return Err(invalid_value(
            "model_route.id",
            value_text(unexpected),
            "absent for kind api",
        ));
    }
    let family = match text.family {
        Some(toml::Value::String(family)) => parse_model_route_family(&family)?,
        Some(other) => {
            return Err(invalid_value(
                "model_route.family",
                value_text(&other),
                MODEL_ROUTE_FAMILIES,
            ));
        }
        None => {
            return Err(invalid_value(
                "model_route.family",
                "missing",
                MODEL_ROUTE_FAMILIES,
            ));
        }
    };
    let model = match text.model {
        Some(toml::Value::String(model)) if !model.is_empty() => model.into_boxed_str(),
        Some(other) => {
            return Err(invalid_value(
                "model_route.model",
                value_text(&other),
                "non-empty model id",
            ));
        }
        None => {
            return Err(invalid_value(
                "model_route.model",
                "missing",
                "non-empty model id",
            ));
        }
    };
    let provider = match text.provider {
        None => None,
        Some(toml::Value::String(provider)) if !provider.is_empty() => {
            Some(provider.into_boxed_str())
        }
        Some(other) => {
            return Err(invalid_value(
                "model_route.provider",
                value_text(&other),
                "non-empty configured provider id",
            ));
        }
    };
    Ok(crate::model::ModelRoute::Api {
        family,
        model,
        provider,
    })
}

/// Builds a synthetic or harness route after rejecting the members those
/// kinds do not own.
fn parse_model_route_id(
    text: ModelRouteText,
    kind: &str,
) -> Result<crate::model::ModelRoute, ConfigError> {
    use crate::model::ModelRoute;
    if let Some(unexpected) = text.family.as_ref() {
        return Err(invalid_value(
            "model_route.family",
            value_text(unexpected),
            "absent unless kind api",
        ));
    }
    if let Some(unexpected) = text.model.as_ref() {
        return Err(invalid_value(
            "model_route.model",
            value_text(unexpected),
            "absent unless kind api",
        ));
    }
    if let Some(unexpected) = text.provider.as_ref() {
        return Err(invalid_value(
            "model_route.provider",
            value_text(unexpected),
            "absent unless kind api",
        ));
    }
    let id = match text.id {
        Some(toml::Value::String(id)) if !id.is_empty() => id.into_boxed_str(),
        Some(other) => {
            return Err(invalid_value(
                "model_route.id",
                value_text(&other),
                "non-empty synthetic or harness id",
            ));
        }
        None => {
            return Err(invalid_value(
                "model_route.id",
                "missing",
                "non-empty synthetic or harness id",
            ));
        }
    };
    if kind == "harness" {
        ModelRoute::harness(id).map_err(|error| {
            invalid_value(
                "model_route.id",
                error.to_string().into_boxed_str(),
                "dalgon/normal, dalgon/eval-first, dalgon/eval-only",
            )
        })
    } else {
        ModelRoute::synthetic(id).map_err(|error| {
            invalid_value(
                "model_route.id",
                error.to_string().into_boxed_str(),
                "[a-z0-9-]+/[a-z0-9._-]+",
            )
        })
    }
}
