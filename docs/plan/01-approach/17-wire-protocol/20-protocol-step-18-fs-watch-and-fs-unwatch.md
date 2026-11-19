#### Protocol step 18. `fs/watch` and `fs/unwatch`

- `serve_fs.go`: `fsWatch`, `fsUnwatch`; per-connection `watches map[string]context.CancelFunc`, at most 64; one goroutine per watch polls every second (a file: its own size, mode and modification time; a directory: the same for each direct child, at most 10000 entries; across all watches, one poll stats at most 20000 entries, and a watch over that budget polls every 5 seconds) and sends `fs/changed` with the absolute paths whose (size, mode, modification time) tuple changed since the last poll, or that appeared or vanished (at most 1000 per message); stopped in `shutdown`.
- Test `internal/rpc` `TestFsWatchReportsChanges` (watch a temp directory, write `x.txt`, receive `fs/changed` naming it within `waitLimit`; unwatch unknown id fails `unknown watch <id>`). Fails before: `-32601`.
- Docs: `protocol.md`.

Anchor: Christopher Alexander.

