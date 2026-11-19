#### Specifications step 3. Correct `docs/spec/approval-rules.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/approval-rules.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 7 | It MUST return the first token and the second token when that token does not start with `-`. | It MUST return the first token and the second token, including a second token that starts with `-`. | internal/agent/rules/rules.go:199-201 `prefix := quoteToken(tokens[0])` / `if len(tokens) > 1 {` / `prefix += " " + quoteToken(tokens[1])`; internal/agent/rules/grant_prefix_flag_test.go:7-8 `GrantPrefix("git -C x status")` must return `"git -C"` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 9 | An unterminated quote or a trailing backslash is malformed. | internal/agent/rules/rules.go:61-64,66-96 (state that the scanner keeps at end of input); rules.go:218-230 (`tokenize` discards it). The sentence defines the term that the test of Spec-fix step 1 relies on. |

Structure:

- Dropped entry below. Resolved, no change: the rules package applies `Check` to exec only (internal/agent/gate.go:142-149) is already stated in approvals.md:28.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 5 | `recursive-delete` MUST match an `rm` invocation with a recursive flag. A recursive flag is `-r`, `-R`, or `--recursive`, combined with other short flags or separate, in any flag position. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 1 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/approval-rules.md` prints `style findings: 0`, and `just verify` passes.

