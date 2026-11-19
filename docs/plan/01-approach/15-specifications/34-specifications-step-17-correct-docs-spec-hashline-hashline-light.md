#### Specifications step 17. Correct `docs/spec/hashline/hashline-light-v1.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/hashline/hashline-light-v1.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 3 | Status: reviewed proposal; not implemented or selected by default. | Status: implemented as an explicit edit style. The default style is `anchor`. | internal/tool/patch/ir.go:11 `HashlineLight Style = "hashline-light"`; internal/config/merge.go:444 `validEditStyle` accepts hashline-light; internal/config/config.go:261 default anchor |
| 18 | For each effective replacement or CUT, including source-node operations: | For each effective replacement or CUT: | internal/tool/patch/hashline.go:119-120 and 293-294 refuse `*`; hashline.go:179-180 `block ops (N*) are not supported; use a line range` |
| 61 | Syntax, mapping, gaps, N*, NEW, REM, MV, byte preservation, output, replay, and limits follow the common contract unchanged. | Syntax, mapping, gaps, NEW, REM, MV, byte preservation, output, replay, and limits follow the common contract. The parser refuses `N*` operations with `patch: line N: block ops (N*) are not supported; use a line range`. | internal/tool/patch/hashline.go:179-180 `blockOpError`; hashline.go:119-120; hashline.go:293-294 |
| 79 | The symbols-on description is: | The model-facing description is: | internal/tool/patch/description.go:11 lightDescription has no symbols variant |
| 85 | snapshot gaps. N* operations require enabled symbol support. | snapshot gaps. | internal/tool/patch/description.go:11 (lightDescription continues 'snapshot gaps.\n[PATH#NEW] plus PUT >$: creates a file.'); profile_test.go:421-422 |
| 97 | With symbols off, remove the N* sentence and its grammar productions. Keep all other semantics identical. Shared quoting and byte-framing rules still apply. | Shared quoting and byte-framing rules apply to this description. | internal/tool/patch/description.go:11 ends at 'retry blindly.' |
| 105 | \| L-02 \| Same range after unrecorded external drift \| needs_context/history_gap; no write. \| | \| L-02 \| Same range after unrecorded external drift \| Error ends with `is stale: snapshot: stale reference. Read again`. No target is written. \| | internal/tool/snapshot/store.go:314, 317, 328 return ErrStaleReference (store.go:23); internal/tool/snapshot/lineage_test.go:224-225 asserts ErrStaleReference for an external change; internal/tool/patch/resolve.go:349-350 wraps it |
| 107 | \| L-04 \| Recorded insertion inside that range \| conflict/target_changed. \| | \| L-04 \| Recorded insertion inside that range \| Error ends with `is stale: snapshot: target changed. Read again`. No target is written. \| | internal/tool/snapshot/store.go:24 `ErrTargetChanged = errors.New("snapshot: target changed")`; store.go:280 returns it; lineage_test.go:184-185 asserts it for an insertion inside the range; resolve.go:349-350 wraps it |
| 108 | \| L-05 \| Unseen source node with valid source parse and preserved extent \| Preparation succeeds. \| | \| L-05 \| Source-node operation `N*` \| Error: `patch: line N: block ops (N*) are not supported; use a line range`. \| | internal/tool/patch/hashline.go:179-180; hashline.go:293-294 |
| 109 | \| L-06 \| Source-node evidence expired \| needs_context; no current-AST guess. \| | Delete the statement. | internal/tool/patch/hashline.go:293-294 refuses N* before any node evidence is read |
| 111 | \| L-08 \| Target valid but authorization denied \| rejected/permission_denied. \| | \| L-08 \| Target valid but approval declined \| Decline with reason `declined by user`. \| | internal/agent/gate.go:317 and 319 `return decline("user", "declined by user")`; grep of internal for permission_denied matches only HTTP status mapping in internal/model/cursor/connect.go:32 and internal/model/devin/connect.go:28 |

Use one word for one thing.

- Keep `delivered`. Replace displayed at lines 29, 36.

Structure:

- Refusals are Go error strings (internal/tool/patch/resolve.go:350, 361, 368, 384) and one decline reason (internal/agent/gate.go:317). L06 names these texts.
- No code change is needed: Light waives coverage only (resolve.go:216), and the Covers path is Enhanced only; stale approval still blocks writes (writer.go:89-90).

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 3 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/hashline/hashline-light-v1.md` prints `style findings: 0`, and `just verify` passes.

