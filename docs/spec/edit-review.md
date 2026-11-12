# Edit review

The edit review judges each edit on one diff.

## Scope

The edit review judges the edit calls of every thread of a dalgona host when `[edit_review] enabled` is true. The key is dalgona only, and `dal` MUST reject it.

A call is judged only when its tool has edit class, it completed with `ok` true, and it changed a file. The judge runs in the background during the round, and the round waits for every judge before it ends.

The review MUST NOT judge a call that recovery runs or replays.

## Diff

The package `internal/textdiff` holds one function, `Unified`. `Unified(oldName, newName string, pre, post []byte, context int)` returns the diff text, a `Stats` value, and an error. `Stats` holds `Added` and `Removed`. `Added` counts the `+` lines of all hunks. `Removed` counts the `-` lines of all hunks.

The package is built on `github.com/aymanbagabas/go-udiff v0.4.1`. The diff text starts with `--- <old label>` and `+++ <new label>`. Callers MUST pass `a/<path>`, `b/<path>`, or `/dev/null` as the labels. Each hunk starts with a `@@` header. Equal inputs return an empty text and a zero `Stats`.

The library writes `\ No newline at end of file` after a line without a final newline. The function MUST NOT write its own marker.

`textdiff.Limit` is `1 << 20` bytes. A side above `textdiff.Limit` MUST NOT be diffed. The caller MUST skip that file.

The edit unit MUST pass 3 for `context`. The turn unit of the edit review MUST pass 0.

## Judging the edit unit

The judge MUST NOT run for an edit call that has no added line. The judge MUST NOT run during recovery.

A unit MUST send its requests to the judge as parts. Each request MUST encode to at most 30000 bytes. A unit MUST send at most 8 parts, and at most 4 requests run at once.

The unit MUST pack whole hunks in call order. A hunk that does not fit an empty part MUST be cut at a blank line, and the pieces MUST recompute their headers. Hunks that do not fit in 8 parts MUST be omitted. A truncated unit MUST NOT be clean, so a clean result MUST be recorded as uncertain. A flagged result MUST stay flagged.

Each request MUST be answered for each question it asks. A missing or out-of-range answer MUST fail its part.

A failed part MUST add no scores. A unit with some answered parts MUST take the worst verdict of the answered parts, and a clean verdict MUST become uncertain. A unit MUST be an error only when every part fails.

A flag MUST speak only when its question is a speaker and the `sound` score of its part is below 0.5.

The edit unit MUST wait at most 8 seconds for all its parts. A timeout MUST fail the parts that have not answered. The review MUST NOT block the edit.
The 8 second bound MUST apply per round, so a turn can wait 8 seconds in each round with edits.

The masked state MUST hold no raw secret. The unit MUST mask the paths and the post-image before the judge sees them.

## Questions

The judge MUST ask `sound` for every edit unit. It MUST ask each other question only when a file of one of its kinds adds lines and the question's condition holds.

The file kind comes from the extension of the path. Each extension MUST belong to one kind.

| Kind | Extensions |
|---|---|
| `interface` | `.css`, `.scss`, `.sass`, `.less`, `.svelte`, `.vue`, `.jsx`, `.tsx`, `.html`, `.htm` |
| `config` | `.toml`, `.yaml`, `.yml`, `.json`, `.ini`, `.env`, `.lock` |
| `text` | `.md`, `.mdx`, `.txt`, `.rst` |
| `code` | every other extension, and a path with no extension |

| Question | Kinds | Condition |
|---|---|---|
| `sound` | all | none |
| `broken_syntax` | code, interface, config | none |
| `lost_code` | all | none |
| `wrong_logic` | code, interface | none |
| `placeholder` | all | none |
| `off_task` | all | the task is not empty |
| `needless_layer` | code, interface | none |
| `narration` | code, interface | none |
| `unguarded_state` | code, interface | an added line matches the concurrency pattern |
| `wasted_work` | code, interface | none |
| `unsafe_input` | code, interface, config | none |
| `unclear_contract` | code, interface | none |
| `off_token` | interface | none |
| `layout_motion` | interface | none |

## Verdicts

A score of 0.9 or more MUST flag its question. A score from 0.5 up to 0.9 MUST make its part uncertain. A question score below 0.5 MUST NOT change the verdict.

The `sound` score MUST NOT flag a question. It MUST only decide whether a flag speaks.

A part is flagged when one of its questions is flagged. A part is uncertain when no question is flagged and one is uncertain. Otherwise the part is clean.

## Journal

A `tool.judged` event MUST name the call, the turn, the judge, the verdict, and the masked paths. It MUST carry `elapsed_ms`.

A `tool.judged` event MUST carry `scores` unless its verdict is `error`. It MUST carry `error` only when its verdict is `error`. It MUST carry `flags` only when its verdict is `flagged`.

The `parts` member MUST count the requests of the unit. It MUST be absent when the count is 1. The `failed_parts` member MUST count the failed requests. It MUST be absent when no request failed.

The `truncated` member MUST be true when the unit omits hunks, leaves out a large file, or stops loading. It MUST be true when a part fails.

## Failure

A judge error, a timeout, a missing answer, or an out-of-range answer MUST fail its part. A failed part MUST NOT fail the edit call, and the call MUST keep its `ok` result.

A factory error MUST give an error verdict for the unit. A unit MUST be an error only when every part fails.

## Feedback

A flagged edit MUST add one feedback block after the results of its round. The block MUST sit at the journal position of its `tool.judged` event, in this form:

```text
<edit-review call="<call id>" paths="<paths>">
An automatic review of this edit found possible problems:
- <one sentence for each spoken question, in journal order>
Check the changed lines. If a problem is real, fix it. If not, ignore this review.
</edit-review>
```

A turn review MUST add one block at the journal position of its `turn.judged` event, in this form:

```text
<turn-review turn="<turn id>">
An automatic review of this turn found possible problems:
- <one sentence for each spoken question, in journal order>
Check the changed files. If a problem is real, fix it. If not, ignore this review.
</turn-review>
```

The `paths` attribute MUST list the masked paths of the call, joined with `, `. Every attribute value MUST be HTML-escaped.

A review that covered part of the edit MUST add the line `This review covered part of the edit.` after its bullets. A review that covered the whole edit MUST NOT add that line.

A clean verdict MUST add no block. Each earlier message MUST keep its bytes in the next request, so the provider prompt cache still matches.

A rebuilt request MUST read the `spoken` ids from the journal, not from the current `feedback` setting.
