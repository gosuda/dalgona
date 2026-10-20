# Providers, models, and thinking levels

This page covers the OpenAI Chat Completions, OpenAI Responses, OpenAI Codex, and Anthropic API families. Codex signs in with a ChatGPT account. `dalgon login` lists every provider you can sign in to. You sign in to a provider, pick a model, and set how hard the model thinks.

Use this page to connect a provider, choose a default model, and set a thinking level.

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
