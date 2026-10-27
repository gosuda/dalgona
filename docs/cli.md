# dalgon command-line reference: flags, subcommands, exit codes

`dalgon` is the dal coding agent. `dal` and `dl` run the same program. With no subcommand and no prompt, it opens the terminal UI on a terminal. With `-p` or `--json`, it runs one turn and exits. Use this page to look up a flag, a subcommand, an exit code, or an environment variable. Run `dalgon --help` for the flag list. Run `dalgon <subcommand> --help` for one subcommand.

## Run one turn

```sh
dalgon -p "Summarize the staged diff"
dalgon --json -o reply.md "Summarize the staged diff"
git diff | dalgon -p - "Review these changes"
```

These runs need a configured model. Without one, dalgon reports the missing model and how to set it.

Print mode (`-p`) runs one turn. Then it waits up to 3 seconds for its extension status kinds to go quiet. JSON mode (`--json`) writes a result as one JSON line. It writes the line only after the turn ends and every extension status kind is quiet. A run that leaves an extension busy delays the result line.

If a kind stays busy, print mode reports `dalgon: extension status is not quiet: <extensions> still busy after 3 seconds` on stderr. It exits 1 if a kind remains busy after the quiet wait. JSON mode has no time limit. It waits for quiet or a stop signal. A stop ends the run without a JSON result line.

With `--approval ask` and no terminal, a headless run denies `patch` and `exec` calls. Use `--approval edits` to approve edits without asking. Use `--approval all` to approve every call, and only in a run that an external sandbox already confines.

## Root flags

A flag overrides the matching key in `dal.toml` for one run. See [Settings in dal.toml](config.md).

| flag | meaning |
|---|---|
| `-p`, `--print` | Print the assistant response and exit. |
| `--json` | Print one JSON result object or error object. Exit 1 on error. |
| `-o FILE`, `--output-last-message FILE` | Write the last assistant message to FILE after a successful run. |
| `-c`, `--continue` | Continue the newest session in this workspace. |
| `-r [SESSION]`, `--resume [SESSION]` | Resume a session by id or name. Without SESSION, open the session picker. |
| `-n NAME`, `--name NAME` | Name the session. |
| `-m ID`, `--model ID` | Use model ID for this run. ID may name a provider route, an alias, a registered synthetic model, or a dalgon mode. |
| `--mode MODE` | Set the harness mode: `normal`, `eval-first`, or `eval-only`. |
| `--thinking THINKING` | Set the reasoning level: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`. |
| `--approval APPROVAL` | Set the approval policy for `patch` and `exec`: `ask`, `edits`, or `all`. |
| `--sandbox` | Turn on the sandbox for this run. |
| `--no-session` | Do not persist this session. |
| `--screen SCREEN` | Select the terminal screen: `inline` or `fullscreen`. |
| `--color COLOR` | Select terminal color: `auto`, `always`, or `never`. |
| `-C DIR`, `--cd DIR` | Use DIR as the workspace without changing the process working directory. |
| `--connect ADDR` | Connect to a remote dalgon host and run the interactive terminal UI. Without it, dalgon uses a host in this process. |
| `--token-file FILE` | Read the bearer token from FILE. dalgon sends it only to non-loopback WebSocket endpoints. Needs `--connect`. |
| `PROMPT...` | Attach text, image, or piped standard-input content to the first user message. A prompt of `-` reads piped standard input. |
| `-h`, `--help` | Print help. |
| `-V`, `--version` | Print the version. |

Some flags cannot combine:

- `--mode` and a `dalgon/` mode id in `--model` cannot combine. Pass the mode once.
- `--continue` and `--resume` cannot combine.
- `--connect` needs the terminal UI. Do not pass it with `--print` or `--json`.

## Subcommands

`dalgon` has 12 subcommands.

| subcommand | what it does |
|---|---|
| `login` | Sign in to a provider or inspect sign-in state. |
| `logout` | Sign out from a provider. |
| `models` | List models with built-in prices. |
| `docs` | Read a documentation page. |
| `serve` | Start the local control server or create its token. |
| `rpc` | Serve the JSON-RPC protocol on standard input and output, or on a local socket. |
| `acp` | Serve the Agent Client Protocol on standard input and output. |
| `app-server` | Serve the Codex app-server protocol on standard input and output. |
| `completion` | Generate shell completion scripts. |
| `plugin` | Manage configured plugin grants. |
| `rules` | Inspect or test stream rules. |
| `dev` | Inspect and drive sessions for debugging. |

A hidden `__sandbox` subcommand is the helper that the sandbox launches. Do not run it by hand.

## dalgon login

Sign in to a model provider. Then run `dal` and pick a model.

```sh
dalgon login
printf %s "$KEY" | dalgon login anthropic --api-key
dalgon login status
```

Without PROVIDER, `dalgon login` shows a provider picker. The interactive CLI prefers browser sign-in first, then device authorization, then an API key prompt. Terminal sign-in lists each sign-in method for providers that offer several, so device code stays selectable without flags. `--api-key` reads an API key from standard input. `--device-auth` uses the provider device flow, where the provider supports it.

Providers with browser OAuth:
- OpenAI Codex opens the sign-in page in your browser and completes via a local loopback callback or a pasted redirect URL, falling back to device authorization if the callback port is busy. Device code can also be chosen directly from the terminal sign-in methods or with `--device-auth`.
- Anthropic and GitLab Duo open the sign-in page in your browser and complete either via local loopback callback or by pasting the authorization code or redirect URL. An empty line leaves the paste prompt open; the browser callback can still complete sign-in.

Device-code sign-in binds no local callback port: dalgon requests a code, displays the server's verification URL and user code to complete in a browser on any device, and polls for approval. During CLI device polling, pressing Ctrl+C cancels authorization and restores the terminal.
- Factory Droid (`factory-droid`) automatically uses this device-code flow without `--device-auth`, polling WorkOS and verifying the session through whoami.
- GitHub Copilot (`github-copilot`) device sign-in prompts for an optional GitHub Enterprise domain; pressing Enter on a blank line chooses `github.com`.

Credentials live in `auth.json` under the data root, with mode `0600` (owner read and write only; dal refuses symbolic links and group or other permission bits). Stored secrets and tokens are never echoed to standard output or error.

Run `dalgon login status` to inspect stored credentials and configured API keys. The command exits 0 when at least one provider is ready. If all credentials are missing or expired, it exits 1. It prints a sign-in hint. OAuth rows show their expiry date. The output does not include tokens or API keys.

## dalgon logout

Remove stored credentials.

```sh
dalgon logout
dalgon logout anthropic
```

`dalgon logout` removes the credentials of every provider and prints `Removed all credentials.`. `dalgon logout PROVIDER` removes one provider and prints `Removed credentials for PROVIDER.`. When nothing is stored, it prints `No stored credentials.`.

## dalgon models

List models with built-in prices, the price source, and the fetch date.

```sh
dalgon models
dalgon models claude
dalgon models --json
```

PATTERN keeps the rows whose provider or id contains it. The table has the columns `PROVIDER`, `ID`, `CONTEXT`, and `PRICE`. It ends with a `Prices: <source>, fetched <date>.` line. `--json` prints one JSON array. Each row has `provider`, `id`, and `context`.

## dalgon docs

Read the embedded manual. Usage: `dalgon docs [URI]`. With no URI, it prints the index. The first index line is `# dal://`. With a page URI, it prints the page. Unknown URIs exit 1 with a miss message on stderr and nothing on stdout.

```sh
dalgon docs
dalgon docs dal://config
```

A mistyped page name prints two lines on stderr. The first is `dalgon: no dal document at <uri>`. The second is `Did you mean <nearest page>? Run dalgon docs for the list.`.

## dalgon serve

Start the model router. It listens on your machine only, unless you pass `--public` with a token.

```sh
dalgon serve
dalgon serve token
dalgon serve --public
dalgon serve --a2a
```

`--a2a` enables the agent-to-agent surface. `--bind ADDR` and `--port PORT` override `serve.bind` and `serve.port`. `--token-file FILE` reads the token from FILE. `--public` needs a token. It serves plain HTTP, so the token crosses the network in plain text.

`dalgon serve token` creates the bearer token and prints it once. It refuses to replace an existing token. `--force` replaces it. Every client must then use the new token. See [dalgon serve](serve.md).

## dalgon rpc

Serve the JSON-RPC protocol on standard input and output. Each line holds one JSON object.

```sh
dalgon rpc
dalgon rpc --socket /tmp/dalgon.sock
```

`--socket [FILE]` listens on a local socket instead. FILE defaults to `rpc/dal.sock` under the data root. See [the dal JSON-RPC protocol](protocol.md).

## dalgon acp

Serve the Agent Client Protocol on standard input and output. Each line holds one JSON-RPC message. Point an ACP-capable editor at this command.

```sh
dalgon acp
```

## dalgon app-server

Serve the Codex app-server protocol on standard input and output.

```sh
dalgon app-server
```

## dalgon completion

Print a shell completion script. SHELL is `bash`, `elvish`, `fish`, `powershell`, or `zsh`.

```sh
dalgon completion zsh
```

## dalgon plugin

Manage plugin grants.

```sh
dalgon plugin list
dalgon plugin grant quality
dalgon plugin revoke quality
```

`list` shows configured plugins and their persistent grants. `grant NAME` grants the capabilities that plugin NAME declares. `revoke NAME` revokes every persistent grant for NAME. List a plugin in `plugins` in `dal.toml` before you grant it. Session grants end with their session. The name `quality` is the plugin from [Write a first plugin](plugins.md#write-a-first-plugin).

## dalgon rules

List the effective stream rules and any skipped-rule problems. Then test a rule against one input.

```sh
dalgon rules
dalgon rules test "check this text"
dalgon rules test --source tool --tool patch --path src/main.rs "let x = 1;"
```

`--source` selects the input stream: `text` (the default), `thinking`, or `tool`. For `tool`, `--tool` names the tool (default `patch`) and `--path` sets the path context. A TEXT of `-` reads at most 1048576 bytes from standard input. See [Rules](rules.md).

## dalgon dev

Inspect and drive sessions for debugging.

```sh
dalgon dev run scenario.jsonl
dalgon dev fold journal.jsonl
dalgon dev journal replay journal.jsonl
```

- `run FILE` drives a scripted headless session from a scenario file. Each nonblank line holds one step as a JSON object. `--keep` keeps the data root and workspace of the run. `--root DIR` reuses an earlier kept run root. `--consent` lets steps that authorize actions act with your privileges. Without it, the scenario file alone cannot grant them.
- `fold FILE` prints which folded fields each journal record changes.
- `journal replay FILE` folds a journal and prints the recovered state.
- `journal diff BEFORE AFTER` prints the folded fields that differ between two journals.
- `journal torn IN OUT` copies a journal with its tail cut mid-record.
- `journal sidecar DIR [NAME]` lists the sidecar files in a session directory, or dumps one.

## Files and environment

The config file is `$XDG_CONFIG_HOME/dal/dal.toml`, or `~/.config/dal/dal.toml` when `XDG_CONFIG_HOME` is unset. The data root is `$XDG_DATA_HOME/dal/`, or `~/.local/share/dal/`. `dalgona` uses `dalgona/` in both places. dalgon ignores an `XDG_` path that is not absolute. The layout is the same on every OS. On Windows, `~` is absolute `HOME`, then absolute `USERPROFILE`, then absolute `HOMEDRIVE` plus `HOMEPATH` combined.

| variable | effect |
|---|---|
| `DAL_LOG` | Selects the log level. |
| `DAL_NO_MOTION` | Disables terminal motion. |
| `DAL_DEBUG` | Enables terminal diagnostics. |

Other `DAL_` names do not override config keys. Use `dal.toml` or command-line flags to change settings.

## Exit codes

| code | meaning |
|---|---|
| 0 | success |
| 1 | requested failure |
| 124 | usage error |
| 125 | internal error |
| 130 | interrupted run |
| 141 | broken pipe |

Every error names what happened and the fix.

## Next

- [Quickstart](quickstart.md): sign in and run a first turn.
- [Settings in dal.toml](config.md): set the defaults that these flags override.
- [Tools, modes, and approval](tools.md): choose an approval policy and a mode.


