#### Ask step 6. The `-p` client refuses questions

File: `internal/app/cli.go`. `turnBook.Request` (`:734-744`) gets a first branch for `method == "item/tool/requestUserInput"`:

- Decode `rpc.ToolRequestUserInputParams`.
- Print `declined question <header of the first question>: no interactive client` plus a newline to `b.stderr`.
- Return `errors.New("no interactive client")`. The server sees a non-dismiss error from the only subscriber and returns `TextNoClient` to the model.

Test: `internal/app/turnbook_test.go` (new file, package `app`), `TestTurnBookRefusesQuestion`. It checks the error is not nil and the exact stderr line. It fails before this step: today's text is `unsupported server request item/tool/requestUserInput` and nothing is printed.

Docs: `docs/spec/protocol.md` `## Clients`: "In `-p` mode the client declines every approval request and refuses every question, so the model receives `declined: no client could answer this question`." This step adds only this sentence to `## Clients`.

- Lands after: Step 3 (`rpc.ToolRequestUserInputParams`).

Anchor: Otl Aicher.

