#### Live-debug step 2. Add the `llmstub` command and the live script

Files to add:

- `internal/llmstub/cmd/llmstub/main.go`
- `internal/llmstub/cmd/llmstub/main_test.go`
- `internal/llmstub/testdata/live.json`, the script in the section "Schemas of the live-debug workstream".
- `internal/llmstub/testdata/workspace/`, holding:
  - `a.txt`, one line: `hello from a.txt`
  - `notes.md`, three lines: `# Notes`, `old line`, `end`
  - `src/main.go`, a valid Go program of 20 lines whose `main` prints `hello`
  - `pixel.png`, a 1x1 opaque white PNG, encoded once with `image/png` and committed as bytes

File to update: `docs/spec/live-debug.md`, section "Command".

Symbols:

- `main` calls `os.Exit(run(ctx, os.Args[1:], os.Stderr))` with `signal.NotifyContext(ctx, os.Interrupt, syscall.SIGTERM)`.
- `func run(ctx context.Context, args []string, stderr io.Writer) int` handles the flags:
  - `-listen`, default `127.0.0.1:18080`
  - `-script`, required
  - `-key`, required, non-empty
  - `-record`, optional path, opened with `O_CREATE|O_WRONLY|O_APPEND` and mode `0600`
- `run` serves on `http.Server{ReadHeaderTimeout: 10 * time.Second}` and prints exactly `llmstub: serving http://<bound addr>/v1` to stderr. When the context ends it calls `Shutdown` with a 5 s limit and returns 0.
- Flag errors return 2 with `llmstub: <error>`. A parse or listen error returns 1.
- Write to stderr with `fmt.Fprintf`. Nothing is written to stdout.
- Scenario names. The web workstream names three live scenarios: `markdown-approval-ask`, `long-content` and `many-threads`. `markdown-approval-ask` is the turns `t-md`, `t-patch` and `t-ask`, sent in one thread in that order. The web workstream runs its long-content and many-thread checks as manual steps in the browser session of Step 4 (its section on checks gives each action and observation), so `live.json` needs no turn for them.

Tests that fail before the change:

- `internal/llmstub/cmd/llmstub`:
  - `TestRunRequiresKey` (exit 2, stderr contains `-key`)
  - `TestRunServesAndStopsOnCancel` (`-listen 127.0.0.1:0`; read the bound address from the stderr line, GET `/v1/models` with the bearer token and get 200, cancel the context, `run` returns 0 within 6 s)
- `internal/llmstub`: `TestLiveScriptParses` (`Parse("testdata/live.json", raw)` succeeds, with `raw` read by `os.ReadFile` from that path). The turn-id check is Step 4 of this workstream.

- Lands after: Step 1 of this workstream.

Anchor: Dieter Rams.

