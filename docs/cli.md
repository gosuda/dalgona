# The command line

Run `dalgon --help` for the full flag list. Print mode runs one turn, then waits up to 3 seconds for its extension status kinds to go quiet. JSON mode (`--json`) writes a result as one JSON line only after the turn ends and every extension status kind is quiet, so a run that leaves an extension busy holds the result line back.

Print mode reports `dalgon: extension status is not quiet: <extensions> still busy after 3 seconds` on stderr and exits 1 if a kind remains busy after the quiet wait. JSON mode has no time limit: it waits for quiet or a stop signal; stopping ends the run without writing a JSON result line.

## dal docs

Read the embedded manual. Usage: `dalgon docs [URI]`. With no URI, prints the index. With a page URI, prints the page. Unknown URIs fail with a miss message on stderr and nothing on stdout.

## dal login

Sign in to a model provider, then pick a model.

Run `dalgon login status` to inspect stored credentials and configured API keys.
The command exits 0 when at least one provider is ready. If all credentials are
missing or expired, it exits 1 and prints a sign-in hint. OAuth rows show their
expiry date. The output does not include tokens or API keys.

## dal rpc

Serve the JSON-RPC protocol over stdio or a local socket.

## dal serve

Start the model router and agent-to-agent endpoint. Listens on loopback only; `--public` requires a token for remote clients arriving through a trusted HTTPS/WSS reverse proxy on the same host.

## dal plugin

Manage plugins: list, grant, revoke.

## dal completion

Print shell completions for the named shell.

FILES: config lives under the per-OS config root; data lives under the per-OS data root.

ENVIRONMENT: `DAL_LOG` selects the log level. `DAL_NO_MOTION` disables terminal motion, and `DAL_DEBUG` enables terminal diagnostics. Other `DAL_` names do not override config keys. Use `dal.toml` or command-line flags to change settings.

Exit codes: 0 success, 1 requested failure, 124 usage, 125 internal error; interrupted runs exit 130; broken pipes exit 141. Every error names what happened and the fix.
