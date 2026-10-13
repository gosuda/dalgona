# The command line

Run `dalgon --help` for the full flag list. Print mode runs one turn, then waits up to 3 seconds for its extension status kinds to go quiet. JSON mode (`--json`) writes a result as one JSON line only after the turn ends and every extension status kind is quiet, so a run that leaves an extension busy holds the result line back.

Print mode reports `dalgon: extension status is not quiet: <extensions> still busy after 3 seconds` on stderr and exits 1 if a kind remains busy after the quiet wait. JSON mode has no time limit: it waits for quiet or a stop signal; stopping ends the run without writing a JSON result line.

## dal docs

Read the embedded manual. Usage: `dalgon docs [URI]`. With no URI, prints the index. With a page URI, prints the page. Unknown URIs fail with a miss message on stderr and nothing on stdout.

## dal login

Sign in to a model provider, then pick a model.

## dal rpc

Serve the JSON-RPC protocol over stdio or a local socket.

## dal serve

Start the model router and agent-to-agent endpoint. Listens on your machine only unless `--public` is given with a token.

## dal plugin

Manage plugins: list, grant services.

## dal completion

Print shell completions for the named shell.

FILES: config lives under the per-OS config root; data lives under the per-OS data root. ENVIRONMENT: settings starting with `DAL_` override config keys. Exit codes: 0 success, 1 requested failure, 124 usage, 125 internal error; Interrupted runs exit 130; broken pipes exit 141. Every error names what happened and the fix.
