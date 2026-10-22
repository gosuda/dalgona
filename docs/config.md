# Settings in dal.toml

The file is `dal.toml` under `$XDG_CONFIG_HOME/dal/` (default `~/.config/dal/dal.toml`). Data lives under `$XDG_DATA_HOME/dal/`. Layering: code defaults, then product defaults, then user `dal.toml`, then CLI flags. A later layer replaces an earlier value by key path. `dalgona` reads `dal.toml` under `$XDG_CONFIG_HOME/dalgona/` with data under `$XDG_DATA_HOME/dalgona/`.

The table shows dal's built-in defaults before product TOML, user settings, and CLI overrides. The meaning column identifies dalgona's different defaults. `none` means omit the key, not the TOML string `"none"`. An omitted table still uses the field defaults listed below. Replace `<data-root>` with your product's data directory.

| key | type | default | meaning |
|---|---|---|---|
| `mode` | string | `normal` | harness mode: `normal`, `eval-first`, or `eval-only`; `--mode` overrides it for one run; see [Tools](tools.md) |
| `model` | string | `none` | the default model reference; dal writes it when you pick a model; `-m` overrides it for one run; `model_route` wins when both are set |
| `model_route` | table | `none` | the structured default model; see below |
| `thinking` | string | `medium` | the default reasoning level: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`; `--thinking` overrides it for one run |
| `approval` | string | `ask` | the approval policy for `patch` and `exec`: `ask` asks before each call, `edits` approves edits and asks for `exec`, `all` approves every call; `--approval` overrides it for one run; see [Tools](tools.md) |
| `screen` | string | `inline` | the terminal surface: `inline` keeps terminal scrollback; `fullscreen` uses the alternate screen and replays the settled transcript on exit; `--screen` overrides it for one run; see [Terminal](terminal.md) |
| `theme` | string | `auto` | the terminal colors: `auto` follows the terminal background, `palette` uses the terminal's own palette, or name one of the nine shipped themes, such as `catppuccin-mocha` |
| `sandbox` | bool | `false` | `true` runs every `exec` child through the platform sandbox; `--sandbox` turns it on for one run; see [Sandbox](sandbox.md) |
| `images` | bool | `false` | `true` enables inline images in terminals that support an image protocol; diagrams render as text when images are off |
| `motion` | bool | `true` | `false` turns off repeated terminal motion; the `DAL_NO_MOTION` environment variable turns it off too |
| `compact_ratio` | number | `0.85` | the share of the context window at which compaction starts, from `0.5` to `0.95`; `0.85` means 85 percent; see [Sessions](sessions.md) |
| `edit_style` | string | `anchor` | the patch format; dalgona defaults to `hashline` |
| `guard` | table | `{ enabled = false }` | guard settings; dalgona defaults to `enabled = true`; see [Guard](guard.md) |
| `search_symbols` | bool | `false` | whether search includes symbols; dalgona defaults to `true` |
| `judge` | table | `none` | shared judge settings, including `gate`; readiness checks do not generate a response; see [Judged rules](rules.md#judged-rules) |
| `plugins` | list | `[]` | the names of the plugins to load; each name is a directory under `<data-root>/plugins/` that holds a `plugin.star`; see [Plugins](plugins.md) |
| `aliases` | table | `none` | short names for models; each key is an alias and each value is a non-empty model id; `-m` accepts an alias |
| `serve` | table | `none` | local control endpoint settings; field defaults apply when omitted |
| `serve.bind` | string | `127.0.0.1` | the address the router binds; `--bind` overrides it; an address that is not loopback needs `--public` and a token |
| `serve.port` | number | `7437` | the TCP port the router listens on, from 0 to 65535; `--port` overrides it |
| `serve.token_file` | path | `"<data-root>/serve.token"` | the authentication token file |
| `serve.approval` | string | `ask` | the approval policy for requests that reach the endpoint: `ask`, `edits`, or `all`; it follows `approval` when you leave it unset |
| `serve.origins` | list | `[]` | the browser origins that may open a WebSocket to the endpoint; write each origin exactly; a WebSocket upgrade that carries any other `Origin` header gets `403` |
| `prices` | table | `none` | per-model prices in USD per million tokens, one `prices.<model-id>` table per model; a price set here wins over the compiled price table; each table needs all four rates, and each rate is a number that is not negative |
| `prices.<model-id>.input` | number | `none` | the rate for uncached input tokens; required in each `prices.<model-id>` table |
| `prices.<model-id>.cached_input` | number | `none` | the rate for cached input tokens; required |
| `prices.<model-id>.cache_write` | number | `none` | the `prices.<model-id>.cache_write` setting; `none` keeps the historical fallback to the applicable input rate |
| `prices.<model-id>.output` | number | `none` | the rate for output tokens, not counting reasoning tokens; required |
| `prices.<model-id>.reasoning` | number | `none` | the rate for separately reported reasoning tokens; required; a rate of `0` is a known price |
| `prices.<model-id>.tiers` | list | `[]` | request-wide context tiers in any order; each tier has `size` (input threshold) and optional `input`, `cached_input`, `cache_write`, `output`, `reasoning` overrides |
| `rules` | table | `none` | rule matching and retry settings; field defaults apply when omitted |
| `rules.watch` | bool | `true` | whether to watch stream rules |
| `rules.interrupt` | string | `always` | interrupt mode when a rule does not set its own |
| `rules.repeat` | string | `once` | repeat mode when a rule does not set its own |
| `rules.repeat_gap` | number | `10` | turn gap for `after-gap` repeats |
| `rules.max_retries` | number | `3` | maximum rule interrupts per turn |
| `rules.disabled` | list | `[]` | the rule names to drop before rule sets are built, such as `no-sleep`; a name starts with a letter or digit and has at most 64 characters from letters, digits, `.`, `_`, and `-` |
| `rules.judge` | string | `auto` | `off` excludes judged rules; `auto` includes them only when the judge is ready; `on` requires readiness at session startup; separate from `judge.gate`; see [Judged rules](rules.md#judged-rules) |
| `sandbox_writable` | list | `[]` | extra directories that stay writable under the sandbox; each entry is an absolute path or starts with `~/`, and it must exist; it cannot hold the home directory or overlap the dal config and data roots; see [Sandbox](sandbox.md) |
| `agents` | table | `none` | child-session settings: `enabled` publishes the `agent` model tool, `max_concurrent` caps running children, `max_depth` caps nesting; see below |
| `limits` | table | `none` | the Starlark budgets under `limits.plugins`; see below |
| `models` | table | `none` | provider-layer defaults: `default`, `thinking`, and `aliases`; see below |
| `retry` | table | `none` | provider retry limits: `request_max_retries` and `stream_max_retries`; see below |
| `providers` | table | `none` | one `providers.<id>` table per provider; a built-in id changes that provider and any other id adds a named provider; see below |
| `ask` | table | `none` | holds one key, `timeout`; see `ask.timeout` |
| `ask.timeout` | string | `"30m"` | how long one raised question stays open; clamped into `1m` to `120m` at load, with one startup notice naming the key, the value, and the clamp |
| `compact` | table | `none` | an extension-owned table that dal accepts without checking its keys; no part of dal reads a key from it; use `compact_ratio` to set when compaction starts |
| `rule_sets` | table | `none` | read by dalgona only: `rule_sets.enabled` lists the bundled rule sets to load; dal accepts the table and reads no key from it; see `dalgona://rules` |
| `plugin` | table | `none` | per-plugin settings, one `plugin.<name>` table per plugin; the plugin's `config` schema checks the table when the plugin loads; see [Plugins](plugins.md#read-settings) |
| `eval` | table | `none` | holds one key, `uses`; see `eval.uses` |
| `eval.uses` | list | `[]` | the operations that eval cells may request, such as `tools.read`, `tools.search`, `tools.patch`, `tools.exec`, or an exported `tools.<plugin>.<tool>`; each effect still goes through normal approval, with at most 64 entries and no wildcards or duplicates; empty means eval cells can only compute and cannot act |

`[aliases]` maps model aliases. `[serve]` holds router settings. `[limits]` holds run budgets. `[plugin.<name>]` holds per-plugin settings. `[providers.<id>]` configures named provider endpoints or overrides built-in provider routes.

A `[providers.<id>]` table sets connection and authentication settings:

- `api`: wire API family (`openai_chat`, `openai_responses`, `openai_codex`, `anthropic`, `bedrock`, `cursor`, `devin`, `droid`, `gitlab`, `google-generative-ai`, `google-vertex`, `google-gemini-cli`, `google-antigravity`, `mistral`, `ollama`, or `pi-messages`).
- `base_url`: endpoint base URL.
- `transport`: transport protocol, either `"https"` (default) or `"websocket"` (supported for `openai_responses` and `openai_codex`).
- `key_env`: name of the environment variable that holds the API key. Required for custom named providers unless `auth = "none"`. For built-in providers, `key_env` is optional and overrides the default environment variable.
- `auth`: authorization header style for API keys:
  - `"bearer"`: sends the key in an `Authorization: Bearer <key>` header (default for chat and responses).
  - `"x-api-key"`: sends the key in an `x-api-key: <key>` header (default for Anthropic).
  - `"api-key"`: sends the key in an `api-key: <key>` header.
  - `"none"`: sends no API-key authorization header; the entry carries no API key and builds no credential header. It does not make an OAuth-only family usable without its required credential: such a credential-less row is still refused at request admission.
- `max_concurrent_requests`: maximum simultaneous requests for this provider, between 1 and 64 (default 4).
- `api_version`: the `api-version` query value for an Azure-shaped entry. An entry value wins over `AZURE_OPENAI_API_VERSION`.
- `deployments`: a table mapping catalog model ids to Azure deployment names. An entry value wins model by model over `AZURE_OPENAI_DEPLOYMENT_NAME_MAP`; a model with no mapping is sent under its own id. Quote a model id that holds a dot (`"gpt-5.5" = "..."`), or TOML reads the dot as a table path.

`[model_route]` is the authoritative default model when present. Saving a default writes it beside `model`. `kind = "api"` takes `family`, `model`, and an optional `provider`. The `provider` is the id of the configured provider the model belongs to. `kind = "synthetic"` or `"harness"` takes `id` and no other member. With `provider` set, the default resolves that provider alone. Another provider that lists the same model id is never used. A missing provider is an error. The `model` string stays the older family-qualified spelling and never carries the provider. A table without `provider` carries the same members earlier versions wrote. An earlier dalgon cannot load a `dal.toml` whose `[model_route]` has `provider`. It rejects the unknown member, and one rejected setting makes the whole file fail to load. That version refuses to start and names the config error until you delete the `provider` line. After that, `model` selects by family there.

## Tables with their own keys

These tables take more keys than the table above lists. An unknown key in any of them is an error.

- `[agents]`: `enabled` (default `false`) publishes the `agent` model tool. `max_concurrent` (default `32`) caps running children. `max_depth` (default `1`) caps nesting. Both caps are whole numbers above zero.
- `[limits.plugins]`: the Starlark budgets `load_ticks` (default `1000000`), `load_heap_bytes` (`16777216`), `handler_ticks` (`200000`), `handler_heap_bytes` (`8388608`), `cell_ticks` (`2000000`), `cell_heap_bytes` (`33554432`), and `stack_depth` (`100`).
- `[models]`: `default` is the model that an empty reference selects. `thinking` is a reasoning level (default `medium`). `aliases` maps short names to model references.
- `[retry]`: `request_max_retries` (default `4`) and `stream_max_retries` (default `5`). Each is a whole number from 0 to 100.
- `[providers.<id>]`: `api`, `base_url`, `transport` (`https` or `websocket`), `key_env`, `auth` (`bearer`, `x-api-key`, `api-key`, or `none`), and `max_concurrent_requests` (1 to 64, default `4`). Some providers also take `api_version`, `deployments`, `region`, `project`, `location`, `namespace`, `binary`, or `integration`. A new id needs `api` and `base_url`, and it needs `key_env` unless `auth` is `none`. The id `dal` is reserved.
- `[prices.<model-id>]`: besides the four rates, `cache_write` sets the cache-write rate. Without it, dal prices cache writes at the input rate. `tiers` is a non-empty array of tables for prompts above a context size. Each tier needs `size`, a context token count, and at least one rate: `input`, `cached_input`, `cache_write`, `output`, or `reasoning`. Tier sizes must strictly increase.
- `[ask]`: `timeout` only.

## Azure OpenAI

Azure is a shape on a Chat or Responses entry, not a separate `api` value: an entry with `auth = "api-key"`, an `api_version`, or a `deployments` table is sent through the Azure adapter. The key travels in an `api-key` header, never as `Bearer`; OAuth sign-in is refused for these entries.

For Azure entries, an explicit `base_url` always wins. When you omit it, `AZURE_OPENAI_BASE_URL` takes precedence over `AZURE_OPENAI_RESOURCE_NAME`. The resource name must be one valid DNS label, but dal does not validate it when a higher-priority URL already supplies the endpoint. Blank environment values are ignored. Non-Azure entries ignore these Azure variables.

Configure the built-in OpenAI provider and select a provider-qualified model:

```toml
[providers.openai]
auth = "api-key"
key_env = "AZURE_OPENAI_API_KEY"
api_version = "2025-03-01-preview"

[providers.openai.deployments]
"gpt-5.4" = "my-gpt54-deployment"
```

With `AZURE_OPENAI_API_KEY` and either `AZURE_OPENAI_BASE_URL` or `AZURE_OPENAI_RESOURCE_NAME` in the environment, select `openai/gpt-5.4` (for example with `/model openai/gpt-5.4`). When neither `api_version` nor `AZURE_OPENAI_API_VERSION` is set, Responses defaults to `v1` and Chat to `2024-10-21`.

`dal.toml` strictly refuses supplied secrets. Setting keys such as `api_key`, `key`, or `secret` directly in configuration is rejected as an unknown key at load time. Credentials must come from environment variables named by `key_env` or through `dalgon login`.

Stored credentials live in `auth.json` under your product's data directory (`$XDG_DATA_HOME/dal/auth.json` or `$XDG_DATA_HOME/dalgona/auth.json`), written atomically with mode `0600` permissions (owner read and write only). Non-owner permission bits and symbolic links are refused. Stored entries record raw API keys, OAuth tokens, or structured envelopes; for example, GitHub Copilot stores a JSON envelope holding the token alongside any enterprise domain and discovered API endpoint.

Built-in providers resolve credentials from specific environment variables:
- `COPILOT_GITHUB_TOKEN`: GitHub token for GitHub Copilot (`github-copilot`) authentication.
- `GITLAB_TOKEN`: personal access token for GitLab Duo API key authentication.
- `GITLAB_CLIENT_ID`: overrides the default OAuth client ID for GitLab Duo.
- `GITLAB_REDIRECT_URI`: overrides the loopback redirect URI for GitLab Duo browser OAuth callback.
- `RADIUS_API_KEY`: API key for Radius (`radius`), sent as a bearer token.

Bedrock entries (`api = "bedrock"`) authenticate through captured AWS credential sources (environment variables such as `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, shared credentials and configuration files under `~/.aws/`, SSO tokens, external credential processes, or IAM roles) or a stored bearer token in `auth.json`. Provider readiness verifies credential availability against the effective configured provider family, including configured aliases, without performing network requests or writing to cache.

```toml
plugins = ["todo"]

[providers.custom]
api = "openai_chat"
base_url = "https://api.example.com/v1"
key_env = "CUSTOM_API_KEY"
auth = "api-key"
```

## Write a minimal dal.toml

Set the defaults you would otherwise pass as flags on every run. A flag still wins for one run.

```toml
approval = "edits"
thinking = "high"
compact_ratio = 0.8

[serve]
port = 8080
```

This file approves edits without asking and still asks before `exec`. It raises the reasoning level, starts compaction at 80 percent of the context window, and moves the router to port 8080. Pick a model in the terminal, and dal saves `model` and `[model_route]` for you.

Add a plugin name to `plugins` only after its directory exists under `<data-root>/plugins/`. A plugin that fails to load stops startup. After you write the plugin from [Write a first plugin](plugins.md#write-a-first-plugin), add this line at the top of the file:

```toml
plugins = ["quality"]
```

Every config error names the key and the fix.

## Next

- [Command line](cli.md): override these settings for one run with flags.
- [Guard](guard.md): set `guard.enabled`.
- [Rules](rules.md): set `rules.judge` and the other rule defaults.
