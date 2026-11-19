#### Specifications step 13. Correct `docs/spec/hashline/README.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/hashline/README.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 4 | dal's existing patch engine. They do not implement the feature or change defaults. | dal's existing patch engine. The Go module already accepts both profiles as `edit_style` values. These files do not change the default `edit_style`, which stays `anchor`. | internal/tool/patch/ir.go:11-12 `HashlineLight    Style = "hashline-light"` and `HashlineEnhanced Style = "hashline-enhanced"`; internal/config/merge.go:443-444 `func validEditStyle` lists both values; internal/config/config.go:261 `EditStyle: EditStyleSpec{De |
| 31 | [package-checks.json](package-checks.json) records document/link/source-hash checks. [SHA256SUMS](SHA256SUMS) records the packaged file checksums, excluding itself. | Delete the statement. | Directory listing of docs/spec/hashline (read this run) has eight entries: hashline-enhanced-v1.md, hashline-light-v1.md, hashline-review.md, verification.json, verify_model.py, hashline-common-v1.md, hashline-conformance-v1.md, README.md. Neither package-chec |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 25 | `just hashline-model` fails when the verifier output differs from `verification.json`. When `CI` is `true` and `uv` is missing, `just hashline-model` fails. Otherwise, when `uv` is missing, `just hashline-model` prints `hashline-model: skipped, uv is not installed`. It then exits with status 0. | justfile:32-38: `if ! command -v uv`, the `CI` check with `uv is required in CI and is not installed` and exit 1, the `skipped` message and exit 0. justfile:42-43: `cmp "$out" docs/spec/hashline/verification.json`. Add t |

Structure:

- Line 25: name `docs/spec/hashline` as the directory for the bare path `verify_model.py`, or name the gate `just hashline-model`. The bare path works only from that directory; the justfile runs it from the repository root with the full path (justfile:42).

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/hashline/README.md` prints `style findings: 0`, and `just verify` passes.

