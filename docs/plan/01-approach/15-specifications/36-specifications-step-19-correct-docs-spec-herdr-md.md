#### Specifications step 19. Correct `docs/spec/herdr.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/herdr.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 6 | The client MUST bound each herdr command with a 5 s timeout. | cmd/dalgona/client.go:20 `herdrCallTimeout = 5 * time.Second`; client.go:114-115 `ctx, cancel := context.WithTimeout(base, herdrCallTimeout)` before `w.reporter.Report`; client.go:119-120 same for `SubagentPane`; client. |

Use one word for one thing.

- Keep `subagent`. Replace Line 44 code block: `<child-pane>` becomes `<subagent-pane>` and `<child-thread>` becomes `<subagent-thread>`. Line 47: `The child command` becomes `The subagent command`. Line 53: `child-pane command` becomes `subagent pane command`. at lines 44, 47, 53.

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/herdr.md` prints `style findings: 0`, and `just verify` passes.

