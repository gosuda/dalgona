# The command line

Run `dalgon --help` for the full flag list. Print mode runs one turn, then waits up to 3 seconds for its extension status kinds to go quiet. JSON mode (`--json`) writes a result as one JSON line only after the turn ends and every extension status kind is quiet, so a run that leaves an extension busy holds the result line back.

Print mode reports `dalgon: extension status is not quiet: <extensions> still busy after 3 seconds` on stderr and exits 1 if a kind remains busy after the quiet wait. JSON mode has no time limit: it waits for quiet or a stop signal; stopping ends the run without writing a JSON result line.

## dal docs

Read the embedded manual. Usage: `dalgon docs [URI]`. With no URI, prints the index. With a page URI, prints the page. Unknown URIs fail with a miss message on stderr and nothing on stdout.

## dal login

Sign in to a model provider, then pick a model. Usage: `dalgon login [PROVIDER]`.

Without a provider argument, prompts interactively to select from available providers. The interactive CLI prefers browser sign-in first, then device authorization, then an API key prompt. Terminal sign-in lists each sign-in method for providers that offer several, so device code stays selectable without flags.

Flags:
- `--api-key`: prompts for or reads a piped API key for providers that accept keys.
- `--device-auth`: requests device-code authorization for providers supporting device sign-in.

Providers with browser OAuth:
- OpenAI Codex opens the sign-in page in your browser and completes via a local loopback callback or a pasted redirect URL, falling back to device authorization if the callback port is busy. Device code can also be chosen directly from the terminal sign-in methods or with `--device-auth`.
- Anthropic and GitLab Duo open the sign-in page in your browser and complete either via local loopback callback or by pasting the authorization code or redirect URL. An empty line leaves the paste prompt open; the browser callback can still complete sign-in.

Device-code sign-in binds no local callback port: dalgon requests a code, displays the server's verification URL and user code to complete in a browser on any device, and polls for approval. During CLI device polling, pressing Ctrl+C cancels authorization and restores the terminal.
- Factory Droid (`factory-droid`) automatically uses this device-code flow without `--device-auth`, polling WorkOS and verifying the session through whoami.
- GitHub Copilot (`github-copilot`) device sign-in prompts for an optional GitHub Enterprise domain; pressing Enter on a blank line chooses `github.com`.

Credentials are saved to `auth.json` under your data root with mode `0600` permissions (owner read and write only; dal refuses symbolic links and group or other permission bits). Stored secrets and tokens are never echoed to standard output or error.

Run `dalgon login status` to inspect stored credentials and configured API keys.
The command exits 0 when at least one provider is ready. If all credentials are
missing or expired, it exits 1 and prints a sign-in hint. OAuth rows show their
expiry date. The output does not include tokens or API keys.

## dal logout

Sign out from a model provider and remove stored credentials. Usage: `dalgon logout [PROVIDER]`.

Without arguments, `dalgon logout` removes all stored credentials and deletes `auth.json`. With a provider argument (such as `dalgon logout anthropic`), it removes only that provider's credentials. If the removed provider is used by the configured `model` in `dal.toml`, dalgon prints a warning on stderr. Exits 0 on success.

## dal rpc

Serve the JSON-RPC protocol over stdio or a local socket.

## dal serve

Start the model router and agent-to-agent endpoint. Listens on your machine only unless `--public` is given with a token.

## dal plugin

Manage plugins: list, grant, revoke.

## dal completion

Print shell completions for the named shell.

FILES: config lives under the per-OS config root; data lives under the per-OS data root.

ENVIRONMENT: `DAL_LOG` selects the log level. `DAL_NO_MOTION` disables terminal motion, and `DAL_DEBUG` enables terminal diagnostics. Other `DAL_` names do not override config keys. Use `dal.toml` or command-line flags to change settings.

Provider authentication variables such as `GITLAB_TOKEN`, `GITLAB_CLIENT_ID`, and `GITLAB_REDIRECT_URI` are documented in [Settings in dal.toml](config.md).

Exit codes: 0 success, 1 requested failure, 124 usage, 125 internal error; interrupted runs exit 130; broken pipes exit 141. Every error names what happened and the fix.
