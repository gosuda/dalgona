#### Specifications step 8. Correct `docs/spec/decide.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/decide.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 43 | The adapter does not check that a noul value lies in 0 through 1. | llm.go:157-160 `case TypeNoul: value, err := readNumberToken(decoder); answer.Noul = &value` with no range check; the audit placed this gap at line 35, but the sentence describes the LLM adapter, so it belongs in that se |

Structure:

- Line 33: a refusal is defined as an answer whose type is refusal (add after line 33).

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/decide.md` prints `style findings: 0`, and `just verify` passes.

