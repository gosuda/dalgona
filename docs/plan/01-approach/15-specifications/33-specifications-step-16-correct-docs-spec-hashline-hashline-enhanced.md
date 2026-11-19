#### Specifications step 16. Correct `docs/spec/hashline/hashline-enhanced-v1.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/hashline/hashline-enhanced-v1.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 3 | Status: reviewed proposal; not implemented or selected by default. | Status: implemented as an explicit edit style. The default style is `anchor`. | internal/tool/patch/ir.go:12 `HashlineEnhanced Style = "hashline-enhanced"`; internal/tool/patch/resolve.go:216 `needsCoverage` returns true for HashlineEnhanced; internal/config/merge.go:444 `validEditStyle` accepts hashline-enhanced; internal/config/config.g |
| 19 | For each effective replacement or CUT, including source-node operations: | For each effective replacement or CUT: | internal/tool/patch/hashline.go:119-120 `if strings.Contains(text, "*") { return 0, blockOpError(line) }`; hashline.go:179-180 `block ops (N*) are not supported; use a line range`; hashline.go:293-294 for PUT N*; internal/tool/patch/profile_test.go:421-422 fai |
| 71 | For a known target with unseen rows, return needs_context/unobserved_range with no target writes. Show a current bound header and missing complete rows within shared limits, or exact remaining read ranges. A later explicit request may use the new delivery. Never upgrade the pending request after an internal fetch. | For a known target with unseen rows, the request fails with this error: `patch: changes[N]: lines A-B of PATH need prior observation. Read them, then resend` The request writes no target. A later explicit request may use the new delivery. Never upgrade the pending request after an internal fetch. | internal/tool/patch/resolve.go:384 `return fmt.Errorf("patch: changes[%d]: lines %d-%d of %s need prior observation. Read them, then resend", ...)`; resolve.go:190-192 returns this error before any replacement is produced |
| 76 | For missing history, mark current diagnostic coordinates as mapping=unknown. The caller must identify the target again. Do not offer a retag-only repair that silently reuses old unverified coordinates. | For missing history, the request fails with `patch: reference REF for PATH is stale: snapshot: stale reference. Read again`. The caller must read the file again. Do not offer a retag-only repair that silently reuses old unverified coordinates. | internal/tool/patch/resolve.go:349-350 `patch: reference %s for %s is stale: %w. Read again`; internal/tool/snapshot/store.go:313-317 and 328 return ErrStaleReference (text `snapshot: stale reference`, store.go:23); grep of internal for `mapping=` returned no  |
| 82 | The symbols-on description is: | The model-facing description is: | internal/tool/patch/description.go:13 enhancedDescription has no symbols variant |
| 88 | gaps. N* operations require enabled symbol support. Every existing cell | gaps. Every existing cell | internal/tool/patch/description.go:13 (enhancedDescription text continues 'gaps. Every existing cell'); profile_test.go:421-422 |
| 101 | With symbols off, remove the N* sentence and grammar productions. Shared quoting, framing, and lifecycle contracts remain unchanged. | Shared quoting, framing, and lifecycle contracts apply to this description. | internal/tool/patch/description.go:13 ends at 'blind-retry an uncertain commit.'; hashline.go:179-180 |
| 109 | \| E-02 \| Folded or truncated middle rows \| needs_context/unobserved_range. \| | \| E-02 \| Folded or truncated middle rows \| Error ends with `need prior observation. Read them, then resend`. No target is written. \| | internal/tool/patch/resolve.go:384; internal/tool/snapshot/store.go:157 Covers is the only coverage source; internal/tool/read/read.go:237-238 records only complete rows |
| 116 | \| E-09 \| Concurrent insertion occupies the same gap \| conflict/gap_changed. \| | \| E-09 \| Concurrent insertion occupies the same gap \| Error ends with `is stale: snapshot: gap changed. Read again`. No target is written. \| | internal/tool/snapshot/store.go:25 `ErrGapChanged = errors.New("snapshot: gap changed")`; store.go:298 returns it; internal/tool/patch/resolve.go:361 wraps it as `is stale: %w. Read again`; internal/tool/snapshot/lineage_test.go:194-195 and 201-202 assert ErrG |
| 117 | \| E-10 \| Node moved but its attachment/extent changed \| conflict/node_changed. \| | \| E-10 \| Source-node operation `N*` \| Error: `patch: line N: block ops (N*) are not supported; use a line range`. \| | internal/tool/patch/hashline.go:179-180; hashline.go:293-294; grep of internal for node_changed returned no match |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 29 | Each thread keeps its own snapshot store. Each read mints a new reference, so a patch counts only rows delivered under its own reference. | internal/agent/thread.go:116 `snapshots: snapshot.New()`; internal/agent/tools.go:391 `Snapshots: w.t.snapshots`; internal/tool/snapshot/store.go:118-119 putLocked calls mintLocked; internal/tool/read/read.go:218 `store. |
| 33 | The parser refuses source-node (`N*`) operations with a parse error. | internal/tool/patch/hashline.go:119-120, 179-180, 293-294 |

Use one word for one thing.

- Keep `caller`. Replace consumer at lines 26, 27.
- Keep `history`. Replace lineage at lines 27.
- Keep `delivered`. Replace displayed at lines 43, 51.
- Keep `delivery`. Replace display at lines 118.

Structure:

- Refusals are Go error strings (internal/tool/patch/resolve.go:350, 361, 368, 384), not structured fields. E05 and E07 now name the text.
- No code change is needed: coverage is scoped by the thread store (thread.go:116) and by a reference minted per read (store.go:118-119), and REM stays exempt (resolve.go:216 covers replace only; cut at hashline.go:153 is a replace).

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 2 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/hashline/hashline-enhanced-v1.md` prints `style findings: 0`, and `just verify` passes.

