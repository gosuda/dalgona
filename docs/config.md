# Settings in dal.toml

The file is `dal.toml` under `$XDG_CONFIG_HOME/dal/` (default `~/.config/dal/dal.toml`); data lives under `$XDG_DATA_HOME/dal/`. Layering: code defaults, then product defaults, then user `dal.toml`, then CLI flags. A later layer replaces an earlier value by key path. `dalgona` reads `dal.toml` under `$XDG_CONFIG_HOME/dalgona/` with data under `$XDG_DATA_HOME/dalgona/`.

The table shows dal's built-in defaults before product TOML, user settings, and CLI overrides. The meaning column identifies dalgona's different defaults. `none` means omit the key, not the TOML string `"none"`; an omitted table still uses the field defaults listed below. Replace `<data-root>` with your product's data directory.

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
| `edit_style` | string | `anchor` | the patch format; dalgona defaults to `hashline` |
| `guard` | table | `{ enabled = false }` | guard settings; dalgona defaults to `enabled = true`; see [Guard](guard.md) |
| `search_symbols` | bool | `false` | whether search includes symbols; dalgona defaults to `true` |
| `judge` | table | `none` | shared judge settings, including `gate`; readiness checks do not generate a response; see [Judged rules](rules.md#judged-rules) |
| `plugins` | list | `[]` | the `plugins` setting |
| `aliases` | table | `none` | the `aliases` setting |
| `serve` | table | `none` | local control endpoint settings; field defaults apply when omitted |
| `serve.bind` | string | `127.0.0.1` | the `serve.bind` setting |
| `serve.port` | number | `7437` | the `serve.port` setting |
| `serve.token_file` | path | `"<data-root>/serve.token"` | the authentication token file |
| `serve.approval` | string | `ask` | the `serve.approval` setting |
| `serve.origins` | list | `[]` | the `serve.origins` setting |
| `prices` | table | `none` | the `prices` setting |
| `prices.<model-id>.input` | number | `none` | the `prices.<model-id>.input` setting |
| `prices.<model-id>.cached_input` | number | `none` | the `prices.<model-id>.cached_input` setting |
| `prices.<model-id>.cache_write` | number | `none` | the `prices.<model-id>.cache_write` setting; `none` keeps the historical fallback to the applicable input rate |
| `prices.<model-id>.output` | number | `none` | the `prices.<model-id>.output` setting |
| `prices.<model-id>.reasoning` | number | `none` | the `prices.<model-id>.reasoning` setting |
| `prices.<model-id>.tiers` | list | `[]` | request-wide context tiers in any order; each tier has `size` (input threshold) and optional `input`, `cached_input`, `cache_write`, `output`, `reasoning` overrides |
| `rules` | table | `none` | rule matching and retry settings; field defaults apply when omitted |
| `rules.watch` | bool | `true` | whether to watch stream rules |
| `rules.interrupt` | string | `always` | interrupt mode when a rule does not set its own |
| `rules.repeat` | string | `once` | repeat mode when a rule does not set its own |
| `rules.repeat_gap` | number | `10` | turn gap for `after-gap` repeats |
| `rules.max_retries` | number | `3` | maximum rule interrupts per turn |
| `rules.disabled` | list | `[]` | the `rules.disabled` setting |
| `rules.judge` | string | `auto` | `off` excludes judged rules; `auto` includes them only when the judge is ready; `on` requires readiness at session startup; separate from `judge.gate`; see [Judged rules](rules.md#judged-rules) |
| `sandbox_writable` | list | `[]` | the `sandbox_writable` setting |
| `agents` | table | `none` | the `agents` setting |
| `limits` | table | `none` | the `limits` setting |
| `models` | table | `none` | the `models` setting |
| `retry` | table | `none` | the `retry` setting |
| `providers` | table | `none` | the `providers` setting |
| `ask` | table | `none` | the `ask` setting |
| `ask.timeout` | string | `"30m"` | how long one raised question stays open; clamped into `1m` to `120m` at load, with one startup notice naming the key, the value, and the clamp |
| `compact` | table | `none` | the `compact` setting |
| `rule_sets` | table | `none` | the `rule_sets` setting |
| `plugin` | table | `none` | the `plugin` setting |
| `eval` | table | `none` | the `eval` setting |
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

Every config error names the key and the fix.
