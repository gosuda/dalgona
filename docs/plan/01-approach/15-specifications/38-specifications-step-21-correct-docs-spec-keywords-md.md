#### Specifications step 21. Correct `docs/spec/keywords.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/keywords.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 30 | An opening backtick fence MUST NOT have a backtick in its info string. | internal/keywords/keywords.go:211-212 `func opensFence(mark fenceMark) bool {` / `return mark.fence.marker != '`' \|\| !slices.Contains(mark.rest, '`')` |
| 30 | A closing fence MUST contain only whitespace after its marker. | internal/keywords/keywords.go:215-216 `func closesFence(mark fenceMark, active fence) bool {` / `return mark.fence.marker == active.marker && mark.fence.length >= active.length && onlySpace(mark.rest)` |
| 32 | If another `<` appears before the tag ends, the first `<` MUST stay plain text. | internal/keywords/keywords.go:369 `if r == '<' {` / `return 0, false` in findTagEnd (355); keywords.go:340-342 `end, ok := findTagEnd(input, i)` / `if !ok { return tag{}, false }`; keywords.go:290-292 `parsed, ok := pars |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/keywords.md` prints `style findings: 0`, and `just verify` passes.

