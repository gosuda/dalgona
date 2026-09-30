use std::path::Path;

use super::super::{Config, ConfigError, ConfigProduct};

mod agents;
mod core;
mod edit_style;
mod eval;
mod guard;
mod limits;
mod prices;
mod rules;
mod sandbox;
mod tui;

const DATA_ROOT: &str = "/home/test/dalgon";
const DALGONA_DEFAULTS: &str = r#"
mode = "eval-first"
model = "base/model"
thinking = "low"
approval = "edits"
screen = "fullscreen"
theme = "dusk"
sandbox = true
images = true
motion = false
compact_ratio = 0.75
edit_style = "balanced"
search_symbols = false
plugins = ["first", "second"]
disabled_batteries = ["ask", "search"]
experimental_batteries = ["skills"]

[guard]
enabled = false

[aliases]
fast = "family/base"
slow = "dalgon/normal"

[serve]
bind = "127.0.0.2"
port = 9000
token_file = "state/token"
origins = ["https://default.example"]

[prices."family/model"]
input = 1.0
cached_input = 0.5
output = 2.0
reasoning = 3.0
"#;

fn load(product: ConfigProduct, user_toml: &str) -> Result<Config, ConfigError> {
    Config::load(product, Path::new(DATA_ROOT), "", Some(user_toml))
}

fn parse_enum<T: serde::de::DeserializeOwned>(spelling: &str) -> Result<T, toml::de::Error> {
    let document = format!("value = {spelling:?}");
    let mut table: toml::Table = toml::from_str(&document)?;
    table
        .remove("value")
        .expect("the test document contains value")
        .try_into()
}
