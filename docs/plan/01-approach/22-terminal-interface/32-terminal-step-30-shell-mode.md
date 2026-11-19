#### Terminal step 30. `!` shell mode

- `input.go`: `!` typed on an empty prompt sets `m.input.mode = inputShell`. The input line shows `! ` in the point colour of the thread hue (Design record, The point and the thread hue). The placeholder reads `Shell · enter runs · esc leaves`, which carries the word `Shell`.
- `enter` sends `command/exec {command, cwd: opts.Cwd, timeoutMs: 120000, outputBytesCap: 65536}` (dependency P2) from a `tea.Cmd`. The argv is `["sh","-c",text]`, or `["cmd","/C",text]` when `platformFamily` from `initialize` is `windows` (`internal/rpc/protocol.go:32`, the `PlatformFamily` field, which `initialize` fills from `platform()` at `internal/rpc/serve_misc.go:42`).
- The result arrives as `shellResultMsg{n, command, result, err}`. Its stdout, stderr and error pass through `sanitizeText` before any row or chip uses them, as Step 2 requires for every untrusted entry point. It commits one Measured ledger row (Step 14) with key `shell:<n>`, through `commit(key, lines)`. The row has label `!`, subject `<cmd>`, meta `exit N` and the duration. Its state is `Done` when `exitCode` is 0 and `Failed` otherwise (the word map of Step 14). When `err` is set, the state is `Failed` and the body is the error text. Verbosity applies. The prompt also gains a chip labelled `[Shell #k exit N, L lines]`, where `k` is the count `m.input.shells`, incremented when a command is sent so that `k` equals the key `shell:<k>`, and `L` counts output lines (`line` for one). The chip is stored in `editor.chips` (Step 28) and holds `stdout`, then `stderr` when that is not empty. The field `shells int` is added to `inputState`. The chip expands to `Output of `cmd` (exit N):` followed by a fenced block.
- `esc` on an empty shell line leaves the mode. It never counts toward the rewind gesture (Step 33).
- Symbols that change: `inputShell` (Step 21), `shellResultMsg` (Structure), `commit` (Structure). `commit` takes `(key string, lines []string)`, as Structure defines.
- Failing-first tests:
  - `TestBangRunsCommandExecAndAddsChip` (from the workstream). Input: `!`, then `echo hi`, then `enter`. Expected: one `command/exec` call with argv `["sh","-c","echo hi"]` and the chip `[Shell #1 exit 0, 1 line]` in the prompt. The chip label counts lines and uses `line` for one. Source: the chip label rule of this step.
- Spec: one paragraph (shell mode) in the `Editor` section that Step 19 creates. This step adds no other spec text.
- Reference: oh-my-pi `prompt/custom-editor.ts:57-61` and grok-build `app/dispatch/prompt.rs:784-833` (via the feature reports). Taken: `!` on an empty prompt with a distinct mark. Left: `!!`, `$` Python.
- Lands after: Steps 3, 13, 14, 16, 19, 21 and 28.
- Anchor: Richard Feynman

