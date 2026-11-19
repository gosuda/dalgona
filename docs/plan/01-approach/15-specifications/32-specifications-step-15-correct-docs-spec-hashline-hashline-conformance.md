#### Specifications step 15. Correct `docs/spec/hashline/hashline-conformance-v1.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/hashline/hashline-conformance-v1.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 30 | \| REF-06 \| Two revisions have equal bytes/digest \| Preserve distinct revisions and intervening history. \| | \| REF-06 \| Two revisions have equal bytes/digest \| Preserve distinct revisions and intervening lineage. \| | internal/tool/patch/profile_test.go:40 `func TestHashlineGapLineage`; internal/tool/snapshot/store.go:305 `func (s *Store) lineage(`; docs/spec/hashline/hashline-common-v1.md:196-197 `ends the lineage` |
| 52 | \| MAP-09 \| Current bytes differ from retained head \| End lineage; no synthesized diff history. \| | \| MAP-09 \| Current bytes differ from retained head \| End the lineage. Do not synthesize missing edges. \| | docs/spec/hashline/hashline-common-v1.md:196-197: `A missing edge, detected external drift, uncertain outcome, or inconsistent footprint ends the lineage.` |
| 92 | \| RAW-05 \| No sound line-cell footprint representation \| End lineage; no false narrow mapping. \| | \| RAW-05 \| No sound line-cell footprint representation \| End the lineage. Do not create a false narrow mapping. \| | docs/spec/hashline/hashline-review.md:95: `If no sound line-cell splice exists, end lineage rather than invent a narrow map.` |
| 97 | \| FILE-05 \| MV plus edits succeeds \| Destination reference; old-path refs invalid. \| | \| FILE-05 \| MV after edits in the same file section succeeds \| Destination reference; old-path refs invalid. An operation after MV fails. \| | internal/tool/patch/profile_test.go:337-356 `TestMoveAfterEditInvalidatesOldPath`: edits then `MV b.txt` succeed (lines 342-346), and the old-path reference is refused (lines 353-354). internal/tool/patch/hashline.go:103-104 `if section.renamed && p.style.prof |
| 148 | \| 1 \| Patch: snapshot.rs, ir.rs, resolve.rs, tests.rs \| Reference/revision values, pure mapping/coverage, failing contract tests. \| | \| 1 \| Patch: internal/tool/snapshot/store.go, internal/tool/patch/ir.go, internal/tool/patch/resolve.go, internal/tool/patch/*_test.go \| Reference/revision values, pure mapping/coverage, failing contract tests. \| | Directory listing of internal/tool/patch: no .rs file. internal/tool/snapshot/store.go:157 `func (s *Store) Covers(ref string, first, last int) bool`. |
| 151 | \| 4 \| Patch: styles/hashline.rs, style.rs, ast.rs \| New profiles, source-node resolution, section merge. \| | \| 4 \| Patch: internal/tool/patch/hashline.go, internal/tool/patch/ir.go, internal/tool/patch/description.go \| New profiles, source-node resolution, section merge. \| | internal/tool/patch/hashline.go:179-181 `blockOpError` (N* is not supported); internal/tool/patch/ir.go:154-156 `profile()`; internal/tool/patch/description.go:11-13 profile descriptions. Directory listing: no styles/ directory, no style.rs, no ast.rs. |
| 163 | cargo fmt --check --all cargo clippy --all-targets --all-features --locked -- -D warnings cargo clippy --all-targets --locked -- -D warnings cargo check --locked --no-default-features -p dal-core -p dal-tools cargo test --locked -p dal-tools patch cargo test --locked -p dal-tools --features symbols patch cargo test --locked -p dal-agent cargo test --locked -p dal-store cargo deny --locked check | just verify go test -race -count=1 ./internal/tool/patch/... | justfile:121 `verify: fmt-check vet lint test vuln hashline-model web-check cf-check`; justfile test recipe `go test -race -count=1 ./...`; repository root listing has no Cargo.toml. |
| 180 | IDs. Its report is [verification.json](verification.json). Run: | IDs. Its report is [verification.json](verification.json). Run from `docs/spec/hashline`: | justfile:42 runs docs/spec/hashline/verify_model.py from the repository root. The root listing has no verify_model.py, so the bare command on line 183 fails there. verify_model.py:2-3 docstring: `Run: uv run --no-project --python 3.12 verify_model.py`. |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 184 | `just hashline-model` fails when the verifier output differs from `verification.json`. When `CI` is `true` and `uv` is missing, `just hashline-model` fails. Otherwise, when `uv` is missing, `just hashline-model` prints `hashline-model: skipped, uv is not installed`. It then exits with status 0. | justfile:32-38 and justfile:42-43 (as in README). Add only after Spec-fix step 11 lands, because before it the first sentence is false. |

Use one word for one thing.

- Keep `reference`. Replace token at lines 26, 27, 28.
- Keep `lineage`. Replace history at lines 30, 52.

Structure:

- Name the Go test for each row, or state that none exists yet. No file under internal/tool contains a row ID (grep found no match). internal/tool/patch/profile_test.go:337 `TestMoveAfterEditInvalidatesOldPath` already covers the FILE-05 behaviour, and :140 `TestProfileNewFileCreationBytes` exists for the SYN-07 bytes; their bodies were read only for line 337.
- State the numeric ceilings for MEM-04 and MEM-07, or state that the owner has not set them. The staging ceiling in code is 64 MiB at internal/tool/patch/stage.go:91; no code names a resource_limit label.
- SYN-07 names no rule. Cite docs/spec/hashline/hashline-common-v1.md lines 313-316, which internal/tool/patch/hashline.go:139 and :340-341 implement.

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 2 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/hashline/hashline-conformance-v1.md` prints `style findings: 0`, and `just verify` passes.

