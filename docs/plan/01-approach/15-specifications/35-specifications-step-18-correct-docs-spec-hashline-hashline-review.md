#### Specifications step 18. Correct `docs/spec/hashline/hashline-review.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/hashline/hashline-review.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 24 | Primary source: attached `spec/dalgon-v0-plan.md`, 16,732 physical lines, 2,107,077 bytes. | Primary source: attached `spec/dalgon-v0-plan.md`, kept in this repository at `dalgona-private/spec/dalgon-v0-plan.md`, 16,732 physical lines, 2,107,077 bytes. | dalgona-private/spec/dalgon-v0-plan.md:5340-5358 read this run (S-OWNER content); reader reports 16802 total lines for that file |
| 188 | \| Explicit file-delete exception \| Retained and visible in both descriptions. \| | \| Explicit file-delete exception \| Retained. The Enhanced description states it (`description.go:13`). The Light description does not (`description.go:11`). \| | internal/tool/patch/description.go:13 `REM is exempt from the every-line display rule.`; description.go:11 has no exemption sentence |
| 189 | \| Stale head/tail bypass \| Removed only in new profiles. \| | \| Stale head/tail bypass \| Legacy checks the tag before every locator kind, gaps included. Profiles map head and tail gaps through the snapshot store. \| | internal/tool/patch/resolve.go:128-131 calls proveTag before the switch on locator kind (locatorGap at 136-137); resolve.go:145-147 limits proveTag to guardWholeTag and guardVersion; resolve.go:252-260 targetGap maps gaps through mapReferenceGap |
| 190 | \| No-change failure \| Becomes NoChange only in new profiles. \| | \| No-change failure \| Profiles return `no_change` for a valid exact no-op. The writer returns `no_change` for an empty batch in every style. \| | internal/tool/patch/profile_test.go:113-115 pins no_change for profiles; internal/tool/patch/writer.go:37-38 returns no_change for len(batch.files)==0 with no style check; grep of internal/tool/patch non-test found no legacy no-op error text |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 136 | The current code limits each call to 64 MiB of staged content. Each thread snapshot store holds at most 512 entries and 64 MiB. | internal/tool/patch/stage.go:91 `patch: this call stages over 64 MiB; split it into several calls`; internal/tool/snapshot/store.go:17-18 `maxEntries = 512` and `maxBytes = 64 << 20`; internal/agent/thread.go:116 one sto |

Use one word for one thing.

- Keep `thread`. Replace session at lines 129.
- Keep `no-op`. Replace NoChange at lines 146, 154.
- Keep `profile`. Replace mode at lines 113, 156.
- Keep `reference`. Replace token at lines 105.
- Keep `history`. Replace lineage at lines 74, 95.

Structure:

- Name `just hashline-model` (justfile:29-43) beside the R06 counts. It runs verify_model.py and compares the output with verification.json (justfile:42-43).
- No code change is needed: no safety rule in this record was found broken by the code, and the claims about the 109,673 cases and eight regressions match verification.json.

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 12 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/hashline/hashline-review.md` prints `style findings: 0`, and `just verify` passes.

