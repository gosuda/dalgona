# Providers, models, and thinking levels

Supported configuration API families: `openai_chat`, `openai_responses`, `openai_codex`, `anthropic`, `bedrock`, `cursor`, `devin`, `droid`, `gitlab`, `google-generative-ai`, `google-vertex`, `google-gemini-cli`, `google-antigravity`, `mistral`, `ollama`, and `pi-messages`. Built-in provider sign-in supports OpenAI, OpenAI Codex, Anthropic, Factory Droid, GitLab Duo, GitHub Copilot, and Radius, alongside configured providers and API-key presets. Run `dalgon login`, then pick a model.

Sign-in flows vary by provider:
- OpenAI Codex authenticates through browser OAuth with a local callback, with fallback to device authorization.
- Anthropic and GitLab Duo authenticate through browser OAuth with a local callback or code paste; GitLab Duo also supports personal access tokens via `GITLAB_TOKEN`.
- Factory Droid authenticates through WorkOS device-code sign-in (`dalgon login factory-droid`), which displays the verification URL and user code and polls for completion without binding a local callback port.
- GitHub Copilot authenticates through GitHub device authorization (`dalgon login github-copilot`), prompting for an optional GitHub Enterprise URL or domain (blank for `github.com`), displaying the verification URL and user code, and polling for completion without binding a local callback port. Copilot also accepts a token via `COPILOT_GITHUB_TOKEN` or `dalgon login github-copilot --api-key`. Stored credentials are saved to `auth.json` as a raw token or a structured envelope with the token, enterprise domain, and discovered account endpoint. The sign-in epilogue applies best-effort model policy enablement directly without querying the model catalog at sign-in time.

Ollama (`ollama`) and Ollama Cloud (`ollama-cloud`) speak Ollama's native `/api/chat` NDJSON wire (`api = "ollama"`), not its OpenAI-compatible `/v1` surface. `ollama` defaults to `http://127.0.0.1:11434` and reads `OLLAMA_API_KEY`; a local server takes no key, so set `providers.ollama.auth = "none"`. `ollama-cloud` defaults to `https://ollama.com` and takes its key from `OLLAMA_CLOUD_API_KEY` or `dalgon login ollama-cloud --api-key`. The model list comes from `/api/tags`, and every listed model is probed once with `/api/show`, six at a time, for what it supports: a `thinking` capability opens the reasoning levels (GPT-OSS `low` to `high`, DeepSeek V4 `low`, `high`, `max`, any other self-hosted model `low` to `max`), `vision` opens image input, a model without `tools` is sent none, and the `<architecture>.context_length` member sets the context window. A failed probe keeps the model's row from the last good refresh, and a server with no `/api/show` still lists its models. On Ollama Cloud, `num_predict` never exceeds 65,536, DeepSeek V4 Pro and Flash carry that cap as their output limit, and replayed history leaves out assistant reasoning (GLM 5.2 excepts it); a self-hosted server takes any limit and replays reasoning.

Mistral (`mistral`, `api = "mistral"`) takes `MISTRAL_API_KEY` as a bearer token and speaks the native conversations wire: `POST <base>/conversations` with typed `inputs` entries, `instructions`, `completion_args`, and `store: false`, never Chat Completions. A model whose card says `capabilities.reasoning` accepts `reasoning_effort` `none` and `high` (`zai-glm-5-2`: `none`, `high`, `max`; `zai-glm-5-3`: `low`, `high`, `max`); any other model, and any model id with no card, is sent no reasoning control. Prior reasoning replays as `thinking` chunks only while reasoning is on. When a reasoning run carries a signed chunk (a `thinking` chunk with a non-empty `signature`, the optional nullable string the Conversations schema documents for replaying reasoning across turns), dal keeps the run's chunks verbatim, in arrival order, and replays the full array ahead of the message text on the next turn; a run without a signature emits no replay record and journals as before. As dal policy, not a Mistral mandate, verbatim replay applies only to an assistant message this family and model produced, only while reasoning is on, and only when the stored array still has the recorded shape and text; anything else, including every message from another family or model, replays as one text-only `thinking` chunk.

Radius (`radius`, `api = "pi-messages"`) takes an API key from `RADIUS_API_KEY` or `dalgon login radius --api-key`, sent as a bearer token to `https://radius.pi.dev/v1/messages`. Radius OAuth sign-in is not offered. The model list comes from the gateway's `/v1/config` document, cached like every other provider list. Any backend that speaks the pi-messages wire can be configured with `api = "pi-messages"` and a `base_url` that ends at the path before `/messages`.

Bedrock providers authenticate using captured AWS credential sources (environment variables, shared configuration and credential files, SSO, external credential processes, or IAM roles) or a stored bearer token in `auth.json`. Bedrock does not provide an interactive sign-in flow. `dalgon login status` evaluates readiness across effective configured provider families and aliases, reporting `ready` when an AWS credential source or stored bearer token is present, or `not configured` when missing.

Use this page to connect a provider, choose a default model, and set a thinking level.

Azure OpenAI rides on the `openai_chat` and `openai_responses` families: shape a provider entry as Azure (see [Settings in dal.toml](config.md#azure-openai)) and pick a provider-qualified model such as `openai/gpt-5.4`. The catalog id maps to its Azure deployment through the entry's `deployments` table, falling back to the id itself. Requests carry the key in an `api-key` header rather than as a bearer token.

Stored credentials live in `auth.json` at mode 0600 without secret echoing, saving raw API keys, OAuth tokens, or structured envelopes.

## Sign in

Run the sign-in command in a terminal. It lists the providers you can sign in to. Type the number or the id of one provider.

```sh
dalgon login
```

To sign in without a prompt, name the provider. Pipe the key on standard input:

```sh
printf %s "$KEY" | dalgon login anthropic --api-key
```

```text
Saved Anthropic credentials. Run dal to pick a model.
```

`--api-key` reads the key from standard input. `--device-auth` uses the provider's device flow, where the provider offers one. A provider also counts as signed in when its key variable is set. `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` are two of them.

The `openai-codex` provider signs in with a ChatGPT account. It takes no API key. Passing `--api-key` fails with `dalgon: openai-codex uses ChatGPT sign-in: API keys are not accepted`. Run `dalgon login openai-codex`, or use an OpenAI API key with `dalgon login openai --api-key`.

Check the result. The command prints one row per provider and exits 0 when at least one provider is ready:

```sh
dalgon login status
```

```text
anthropic      ready           api_key
openai         not configured
```

A row reads `ready`, `expired`, or `not configured`. A stored OAuth sign-in shows `oauth, expires YYYY-MM-DD`. When no provider is ready, the command exits 1 and prints `Run dalgon login <provider> to sign in.` on standard error. The output never holds a token or an API key.

Sign out with `dalgon logout [PROVIDER]`. Without a provider, it removes every stored credential.

## Pick a model

List the models. The table shows built-in prices and their source. Pass a pattern to filter the list, or `--json` for JSON.

```sh
dalgon models
dalgon models claude
```

If the list fails, check the network and sign in again with `dalgon login PROVIDER`.

Run `dal` once and pick a model from the list. Dal saves the choice. In a session, type `/model [provider/model]` to switch and `/login` to sign in.

A model id has the form `<family>/<model>`, such as `anthropic/claude-sonnet-5` or `openai-codex/gpt-5.6-luna`. These families are spelled `openai-chat`, `openai-responses`, `openai-codex`, and `anthropic`. To set a default by hand, write it in `dal.toml`. An alias gives a long id a short name:

```toml
model = "fast"
thinking = "high"

[aliases]
fast = "anthropic/claude-sonnet-5"
```

For one run, pass the model and the level on the command line:

```sh
dalgon -m anthropic/claude-sonnet-5 --thinking high -p "Summarize the staged diff"
```

With no model set, dal stops with `dalgon: no model is configured: dal.toml has no model key and --model was not given`. The fix line reads `Run dalgon once and pick a model after sign-in, or pass --model ID. See dalgon models.`

For another endpoint, add a `[providers.<name>]` table. It needs `api` and `base_url`, and `key_env` for the variable that holds the key. [Settings in dal.toml](config.md) lists the `model`, `thinking`, `aliases`, and `providers` keys.

## Set the thinking level

Seven levels exist. The default is `medium`.

| level | meaning |
|---|---|
| `off` | no reasoning |
| `minimal` | minimal reasoning |
| `low` | low reasoning |
| `medium` | medium reasoning |
| `high` | high reasoning |
| `xhigh` | extra-high reasoning |
| `max` | the most reasoning the model supports |

Set the level with `thinking` in `dal.toml`, `--thinking LEVEL` for one run, or `/thinking [level]` in a session. Each API takes the level in its own form. Dal maps the level for each API and clamps it to the limits of the model.

Dal sends a temperature only when the model allows one and the thinking level is `off`. At any other level, dal drops the temperature.

## Know what the providers send

- Once a response has delivered output, no failure retries and no sign-in refresh runs. A transport cut before delivery retries with backoff. The longest single wait is 60 seconds.
- Codex requests send the header `originator: dalgon`.
- A Claude OAuth sign-in adds Claude Code markers to each request. They include the beta headers, the user agent, an `x-app: web` header, and an identity line at the start of the system prompt. API keys and bearer keys never send them.
- Luna Reserve is an opt-in Codex model with the id `gpt-reserve`. It opens only when the included usage of your ChatGPT plan runs out. Then dal prints `note: usage is blocked. Luna Reserve is available: type /model gpt-reserve to switch.` It is never a default model. On an account without it, dal says `Luna Reserve is not available for this account`, and the fix is to switch to another model.

## Next

- [Settings in dal.toml](config.md) lists every config key.
- [Slash commands](commands.md) lists `/model`, `/thinking`, and `/login`.
- [Run the dalgon model router and A2A server](serve.md) serves these models over HTTP.
## Usage, cost, and premium requests

Each turn records token counts and, when the provider reports them, a USD cost and a premium request count. A reported provider cost always wins; otherwise dal computes cost from the resolved price table (configured `prices` entry, then live catalog, then compiled snapshot). Computed cost is `(uncached input × input rate) + (cached input × cached_input rate) + (cache writes × cache_write or input rate) + (output × output rate) + (reasoning × reasoning rate)` per million tokens, with tier overrides applied by request size. `premium_requests` counts provider metered requests: an absent value means unknown for that turn, while `0` means the provider measured zero. Session cost displays stay `unknown` when neither a reported cost nor a resolvable price exists.
