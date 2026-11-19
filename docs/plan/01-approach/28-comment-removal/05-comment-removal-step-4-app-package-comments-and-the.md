#### Comment removal step 4. App package comments and the command-line spec

Anchor: Herbert Simon

- Files: `internal/app/battery/battery.go`, `internal/app/cli.go`, `docs/spec/protocol.md`.
- Change: delete the comment lines of rows 36 to 39. Rows 37 and 39 add the spec text below. `docs/spec/tui.md` does not change.
- Spec text, `docs/spec/protocol.md`: as a new paragraph at the end of `## Clients`: `app.TUIOptions.OnStatus` MUST receive `app.Status` values that `cmd/internal/tuiwire` converts from `tui.Status`. `internal/app` MUST NOT import `internal/tui`, so command entry points set `CLIEnv.TUI`.
- Tests that fail before the change: none. The import rule is enforced by the depguard rule in `.golangci.yml` that denies `internal/tui` to core packages, run by `just lint`.
- Check: `just verify` passes. `grep -n "MUST NOT import" docs/spec/protocol.md` returns the new line.
- Lands after: Comment removal step 3

