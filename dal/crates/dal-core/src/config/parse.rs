use super::partial::{FileText, ParsedLayer};
use super::prices::parse_prices;
use super::values::{
    parse_aliases, parse_approval, parse_bool, parse_edit_style, parse_guard, parse_mode,
    parse_nonempty_string, parse_nonempty_strings, parse_number, parse_rules, parse_screen,
    parse_serve, parse_thinking,
};
use super::{ConfigError, ConfigProduct, TuiConfig, invalid_value, value_text};

pub(super) const DALGON_TOP_LEVEL_KEYS: &[&str] = &[
    "mode",
    "model",
    "thinking",
    "approval",
    "screen",
    "tui",
    "theme",
    "sandbox",
    "images",
    "motion",
    "compact_ratio",
    "edit_style",
    "guard",
    "search_symbols",
    "judge",
    "plugins",
    "aliases",
    "serve",
    "prices",
    "rules",
    "sandbox_writable",
    "agents",
    "limits",
    "models",
    "retry",
    "providers",
    "ask",
    "compact",
    "rule_sets",
    "plugin",
    "eval",
];

pub(super) const DALGONA_TOP_LEVEL_KEYS: &[&str] = &[
    "mode",
    "model",
    "thinking",
    "approval",
    "screen",
    "tui",
    "theme",
    "sandbox",
    "images",
    "motion",
    "compact_ratio",
    "edit_style",
    "guard",
    "search_symbols",
    "judge",
    "plugins",
    "aliases",
    "serve",
    "prices",
    "rules",
    "disabled_batteries",
    "experimental_batteries",
    "sandbox_writable",
    "agents",
    "limits",
    "models",
    "retry",
    "providers",
    "ask",
    "compact",
    "rule_sets",
    "plugin",
    "eval",
];

pub(super) const SERVE_KEYS: &[&str] = &["bind", "port", "token_file", "approval", "origins"];
pub(super) const RULES_KEYS: &[&str] = &[
    "watch",
    "interrupt",
    "repeat",
    "repeat_gap",
    "max_retries",
    "disabled",
    "judge",
];
pub(super) const PRICE_KEYS: &[&str] = &["input", "cached_input", "output", "reasoning"];

pub(super) const KNOWN_KEY_ORDER: &[&str] = &[
    "mode",
    "model",
    "thinking",
    "approval",
    "screen",
    "tui",
    "tui.diagrams",
    "theme",
    "sandbox",
    "images",
    "motion",
    "compact_ratio",
    "edit_style",
    "guard",
    "search_symbols",
    "judge",
    "plugins",
    "aliases",
    "serve",
    "serve.bind",
    "serve.port",
    "serve.token_file",
    "serve.approval",
    "serve.origins",
    "prices",
    "prices.<model-id>.input",
    "prices.<model-id>.cached_input",
    "prices.<model-id>.output",
    "prices.<model-id>.reasoning",
    "rules",
    "rules.watch",
    "rules.interrupt",
    "rules.repeat",
    "rules.repeat_gap",
    "rules.max_retries",
    "rules.disabled",
    "rules.judge",
    "disabled_batteries",
    "experimental_batteries",
    "sandbox_writable",
    "agents",
    "limits",
    "models",
    "retry",
    "providers",
    "ask",
    "compact",
    "rule_sets",
    "plugin",
    "eval",
    "eval.uses",
];
pub(super) fn parse_layer(product: ConfigProduct, text: &str) -> Result<ParsedLayer, ConfigError> {
    let value: toml::Value = toml::from_str(text).map_err(|error| syntax_error(text, &error))?;
    let root = match value {
        toml::Value::Table(root) => root,
        other => {
            return Err(invalid_value("", value_text(&other), "configuration table"));
        }
    };
    for key in root.keys() {
        if !top_level_keys(product).contains(&key.as_str()) {
            return Err(unknown_key(key, product, None));
        }
    }
    let file: FileText =
        toml::Value::Table(root)
            .try_into()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: Box::<str>::from(error.message()),
            })?;
    parse_file_text(product, file)
}

pub(super) fn top_level_keys(product: ConfigProduct) -> &'static [&'static str] {
    match product {
        ConfigProduct::Dalgon => DALGON_TOP_LEVEL_KEYS,
        ConfigProduct::Dalgona => DALGONA_TOP_LEVEL_KEYS,
    }
}

pub(super) fn known_keys(product: ConfigProduct, price_model: Option<&str>) -> Vec<Box<str>> {
    KNOWN_KEY_ORDER
        .iter()
        .map(|key| {
            if let (Some(model), Some(rate)) = (price_model, key.strip_prefix("prices.<model-id>."))
            {
                format!("prices.{model}.{rate}").into_boxed_str()
            } else {
                Box::<str>::from(*key)
            }
        })
        .filter(|key| {
            product == ConfigProduct::Dalgona
                || (key.as_ref() != "disabled_batteries"
                    && key.as_ref() != "experimental_batteries")
        })
        .collect()
}

pub(super) fn unknown_key(
    key: &str,
    product: ConfigProduct,
    price_model: Option<&str>,
) -> ConfigError {
    let known_keys = known_keys(product, price_model);
    let suggestion = closest_key(key, &known_keys);
    ConfigError::UnknownKey {
        key: Box::<str>::from(key),
        suggestion,
        known_keys,
    }
}

pub(super) fn closest_key(key: &str, known_keys: &[Box<str>]) -> Option<Box<str>> {
    let mut closest: Option<(&Box<str>, usize)> = None;
    for candidate in known_keys {
        let distance = edit_distance(key.as_bytes(), candidate.as_bytes());
        if distance > 2 {
            continue;
        }
        match closest {
            Some((_, best_distance)) if distance >= best_distance => {}
            _ => closest = Some((candidate, distance)),
        }
    }
    closest.map(|(candidate, _)| Box::<str>::from(candidate.as_ref()))
}

pub(super) fn edit_distance(left: &[u8], right: &[u8]) -> usize {
    const MAX_DISTANCE: usize = 2;
    if left.len().abs_diff(right.len()) > MAX_DISTANCE {
        return MAX_DISTANCE + 1;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_byte) in left.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];
        for (right_index, right_byte) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(substitution);
            row_min = row_min.min(current[right_index + 1]);
        }
        if row_min > MAX_DISTANCE {
            return MAX_DISTANCE + 1;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

pub(super) fn syntax_error(text: &str, error: &toml::de::Error) -> ConfigError {
    let line = error.span().and_then(|span| {
        text.get(..span.start)
            .map(|prefix| prefix.bytes().filter(|byte| *byte == b'\n').count() + 1)
    });
    ConfigError::Syntax {
        line,
        message: Box::<str>::from(error.message()),
    }
}

pub(super) fn parse_file_text(
    product: ConfigProduct,
    mut file: FileText,
) -> Result<ParsedLayer, ConfigError> {
    let mut layer = ParsedLayer::default();
    capture_raw_sections(&mut layer, &mut file);
    if let Some(value) = layer.sections.get("judge")
        && !matches!(value, toml::Value::Table(_))
    {
        return Err(invalid_value(
            "judge",
            value_text(value),
            "Use a [judge] table with gate, model, timeout_ms, max_concurrent, max_per_turn.",
        ));
    }
    if let Some(value) = file.mode {
        layer.mode = Some(parse_mode(value)?);
    }
    if let Some(value) = file.model {
        layer.model = Some(parse_nonempty_string("model", value, "non-empty model id")?);
    }
    if let Some(value) = file.thinking {
        layer.thinking = Some(parse_thinking(value)?);
    }
    if let Some(value) = file.approval {
        layer.approval = Some(parse_approval("approval", value)?);
    }
    if let Some(value) = file.screen {
        layer.screen = Some(parse_screen(value)?);
    }
    if let Some(value) = file.tui {
        layer.tui = Some(parse_tui(product, value)?);
    }
    if let Some(value) = file.theme {
        layer.theme = Some(parse_nonempty_string("theme", value, "non-empty string")?);
    }
    if let Some(value) = file.sandbox {
        layer.sandbox = Some(parse_bool("sandbox", value)?);
    }
    if let Some(value) = file.images {
        layer.images = Some(parse_bool("images", value)?);
    }
    if let Some(value) = file.motion {
        layer.motion = Some(parse_bool("motion", value)?);
    }
    if let Some(value) = file.compact_ratio {
        let ratio = parse_number("compact_ratio", value, "finite number in 0.5..=0.95")?;
        if !ratio.is_finite() || !(0.5..=0.95).contains(&ratio) {
            return Err(invalid_value(
                "compact_ratio",
                format!("{ratio}"),
                "finite number in 0.5..=0.95",
            ));
        }
        layer.compact_ratio = Some(ratio);
    }
    if let Some(value) = file.edit_style {
        layer.edit_style = Some(parse_edit_style(value)?);
    }
    if let Some(value) = file.guard {
        layer.guard = Some(parse_guard(value)?);
    }
    if let Some(value) = file.search_symbols {
        layer.search_symbols = Some(parse_bool("search_symbols", value)?);
    }
    if let Some(value) = file.plugins {
        layer.plugins = Some(parse_nonempty_strings(
            "plugins",
            value,
            "array of non-empty strings",
        )?);
    }
    if let Some(value) = file.eval {
        layer.eval = Some(super::eval::EvalConfig::parse(product, value)?);
    }
    if let Some(value) = file.aliases {
        layer.aliases = Some(parse_aliases(value)?);
    }
    if let Some(value) = file.serve {
        layer.serve = Some(parse_serve(product, value)?);
    }
    if let Some(value) = file.rules {
        layer.rules = Some(parse_rules(product, value)?);
    }
    if let Some(value) = file.prices {
        layer.prices = parse_prices(product, value)?;
    }
    if let Some(value) = file.disabled_batteries {
        layer.disabled_batteries = Some(parse_nonempty_strings(
            "disabled_batteries",
            value,
            "array of non-empty battery names",
        )?);
    }
    if let Some(value) = file.experimental_batteries {
        layer.experimental_batteries = Some(parse_nonempty_strings(
            "experimental_batteries",
            value,
            "array of non-empty battery names",
        )?);
    }
    Ok(layer)
}

fn parse_tui(product: ConfigProduct, value: toml::Value) -> Result<TuiConfig, ConfigError> {
    let toml::Value::Table(table) = value else {
        return Err(invalid_value("tui", value_text(&value), "table"));
    };
    for key in table.keys() {
        if key != "diagrams" {
            return Err(unknown_key(&format!("tui.{key}"), product, None));
        }
    }
    let diagrams = match table.get("diagrams") {
        None => false,
        Some(toml::Value::Boolean(value)) => *value,
        Some(value) => return Err(invalid_value("tui.diagrams", value_text(value), "boolean")),
    };
    Ok(TuiConfig { diagrams })
}

/// Moves extension-owned raw tables into the layer without validation.
pub(super) fn capture_raw_sections(layer: &mut ParsedLayer, file: &mut FileText) {
    for (name, value) in [
        ("sandbox_writable", file.sandbox_writable.take()),
        ("agents", file.agents.take()),
        ("limits", file.limits.take()),
        ("models", file.models.take()),
        ("retry", file.retry.take()),
        ("providers", file.providers.take()),
        ("ask", file.ask.take()),
        ("compact", file.compact.take()),
        ("rule_sets", file.rule_sets.take()),
        ("plugin", file.plugin.take()),
        ("judge", file.judge.take()),
    ] {
        if let Some(value) = value {
            layer.sections.insert(name.into(), value);
        }
    }
}
