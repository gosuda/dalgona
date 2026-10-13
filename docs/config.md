# Settings in dal.toml

The file is `dal.toml` under `$XDG_CONFIG_HOME/dal/` (default `~/.config/dal/dal.toml`); data lives under `$XDG_DATA_HOME/dal/`. Layering: code defaults, then product defaults, then user `dal.toml`, then CLI flags. A later layer replaces an earlier value by key path. Dalgona reads `dal.toml` under `$XDG_CONFIG_HOME/dalgona/` with data under `$XDG_DATA_HOME/dalgona/`.

| key | type | default | meaning |
|---|---|---|---|
| `mode` | string | `normal` | the `mode` setting |
| `model` | string | `none` | the `model` setting |
| `thinking` | string | `medium` | the `thinking` setting |
| `approval` | string | `ask` | the `approval` setting |
| `screen` | string | `inline` | the `screen` setting |
| `theme` | string | `auto` | the `theme` setting |
| `sandbox` | bool | `false` | the `sandbox` setting |
| `images` | bool | `false` | the `images` setting |
| `motion` | bool | `true` | the `motion` setting |
| `compact_ratio` | number | `0.85` | the `compact_ratio` setting |
| `edit_style` | string | `anchor` | the `edit_style` setting |
| `guard` | table | `none` | the `guard` setting |
| `search_symbols` | bool | `false` | the `search_symbols` setting |
| `judge` | table | `none` | the `judge` setting |
| `plugins` | list | `[]` | the `plugins` setting |
| `aliases` | table | `none` | the `aliases` setting |
| `serve` | table | `none` | the `serve` setting |
| `serve.bind` | string | `127.0.0.1` | the `serve.bind` setting |
| `serve.port` | number | `7437` | the `serve.port` setting |
| `serve.token_file` | path | `none` | the `serve.token_file` setting |
| `serve.approval` | string | `ask` | the `serve.approval` setting |
| `serve.origins` | list | `[]` | the `serve.origins` setting |
| `prices` | table | `none` | the `prices` setting |
| `prices.<model-id>.input` | number | `none` | the `prices.<model-id>.input` setting |
| `prices.<model-id>.cached_input` | number | `none` | the `prices.<model-id>.cached_input` setting |
| `prices.<model-id>.output` | number | `none` | the `prices.<model-id>.output` setting |
| `prices.<model-id>.reasoning` | number | `none` | the `prices.<model-id>.reasoning` setting |
| `rules` | table | `none` | the `rules` setting |
| `rules.watch` | string | `none` | the `rules.watch` setting |
| `rules.interrupt` | string | `none` | the `rules.interrupt` setting |
| `rules.repeat` | number | `none` | the `rules.repeat` setting |
| `rules.repeat_gap` | string | `none` | the `rules.repeat_gap` setting |
| `rules.max_retries` | number | `none` | the `rules.max_retries` setting |
| `rules.disabled` | list | `[]` | the `rules.disabled` setting |
| `rules.judge` | string | `auto` | the `rules.judge` setting |
| `sandbox_writable` | list | `[]` | the `sandbox_writable` setting |
| `agents` | table | `none` | the `agents` setting |
| `limits` | table | `none` | the `limits` setting |
| `models` | table | `none` | the `models` setting |
| `retry` | table | `none` | the `retry` setting |
| `providers` | table | `none` | the `providers` setting |
| `ask` | table | `none` | the `ask` setting |
| `compact` | table | `none` | the `compact` setting |
| `rule_sets` | table | `none` | the `rule_sets` setting |
| `plugin` | table | `none` | the `plugin` setting |
| `eval` | table | `none` | the `eval` setting |
| `eval.uses` | list | `[]` | the operations that eval cells may request, such as `tools.read`, `tools.search`, `tools.patch`, `tools.exec`, or an exported `tools.<plugin>.<tool>`; each effect still goes through normal approval, with at most 64 entries and no wildcards or duplicates; empty means eval cells can only compute and cannot act |

`[aliases]` maps model aliases. `[serve]` holds router settings. `[limits]` holds run budgets. `[plugin.<name>]` holds per-plugin settings.

```toml
plugins = ["todo"]
```

Every config error names the key and the fix.
