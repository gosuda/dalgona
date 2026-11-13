# Edit review

The edit review judges each edit on one diff.

## Scope

The edit review judges the edit calls of every thread of a dalgona host when `[edit_review] enabled` is true. The key is dalgona only, and `dal` MUST reject it.

A call is judged only when its tool has edit class, it completed with `ok` true, and it changed a file. The judge runs in the background during the round, and the round waits for every judge before it ends.

Each edit and turn review MUST read the current saved `enabled` value when it admits work.
A missing judge factory MUST mean that review capability is absent.
Saving `enabled = false` MUST bar later edit and turn reviews, including in open threads.
An admitted review MUST finish normally after review is disabled.
Saving `enabled = true` MUST allow later reviews without restarting the host.
Saved changes MUST NOT remove historical verdicts or their feedback.

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
| `incomplete` | turn unit only | the task is not empty |

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

## Turn review

A turn review MUST run once for each turn, before the turn ends, and only when an edit of that turn passed the pre-filter. It MUST NOT run during recovery, and it MUST NOT run for a turn without a task.

The turn unit MUST ask `sound` and `incomplete`.

A continuation turn MUST be judged as its own unit, with only the edits that it made. A turn id MUST have at most one turn review.

The turn review MUST send the cumulative change of each path, with 0 context lines, so the request holds changed lines only. A diff over the request limit MUST be cut with a truncation marker. Each file MUST take the room left divided by the number of files not yet decided. A file whose diff fits its share MUST keep all of it. The unused share MUST pass to the files after it.

The turn review MUST size its base request with the longest boolean metadata encoding before it shares diff room.

A path whose last change has a post-image digest but no post-image blob MUST be listed in `skipped`. That path MUST NOT be judged. A deletion has no post-image by nature, so its last change MUST be judged. The path of a deletion MUST NOT be listed in `skipped`.

The turn review MUST wait at most 8 seconds for its one request. The turn review MUST NOT speak when it is truncated, but it MUST still journal its verdict. A turn that the user interrupts MUST journal an error verdict and MUST NOT add a message.

## Clients

The server MUST send `dalgona/tool/judged` for each judged edit call, and `dalgona/turn/judged` for each judged turn. Each notification MUST carry the verdict and a `summary`.

The `summary` of a `clean` verdict MUST be `No problems found`. The `summary` of a `flagged` verdict MUST start with `Flagged:` and MUST list the labels of its flags. It MUST add ` (not sent to the model)` when no flag speaks.

The `summary` of an `uncertain` verdict MUST start with `Uncertain:`. The `summary` of an `error` verdict MUST start with `Failed:`, followed by the error text. The error text MUST be at most 120 bytes, cut at a character boundary.

A review that covered part of the diff MUST add `part of the diff was not judged` to a `flagged` or `uncertain` summary.

These notifications are live only. The server MUST NOT return them in thread history.

## Measurement

The corpus holds 472 cases in `internal/agent/testdata/editreview/cases.jsonl`. `ratchet.json` in the same directory records the SHA-256 of that file.

The cases come from the history of this repository at commit `3c4b6f94`.
The current corpus does not meet the required 620 cases or the required group quotas.
The history contains two eligible single-file interface edits. Another 22 interface changes belong to multi-file commits.
Those multi-file changes do not meet the single-file source rule. The required interface quota remains 36.

| Group | Cases | Held out | Source |
|---|---|---|---|
| sound-edit | 108 | 54 | single-file edits that add lines: 60 code, 24 interface, 12 config, 12 text; multi-file commits fill the interface quota |
| sound-turn | 30 | 15 | turns whose subject names a changed file |
| multipart | 40 | 20 | edits that need two or more parts |
| moved | 1 | 0 | the one history commit that meets the different-part packing rule for one top-level Go function |
| 14 defect groups | 293 | 145 | one group for each question id except `sound`; eight groups hold 30 cases, and six short groups hold their pool size |

In each group, the case ids sort by the SHA-256 of each id. Even positions are `dev`, and odd positions are `held_out`. The held-out split holds 234 cases: 89 sound cases and 145 defect cases.

Each defect case takes one single-file sound edit, or one turn whose subject names a changed file, and applies one rule. A subject names a file when the file's base name occurs in the subject as a whole word. The builder is `internal/agent/review_corpus_build_test.go`.

| Question | Rule |
|---|---|
| `broken_syntax` | delete the last added closing bracket, brace, parenthesis or `/>` |
| `lost_code` | delete the first unchanged statement that no hunk touches |
| `wrong_logic` | swap the first comparison or logical operator in an added code line |
| `placeholder` | replace the first added non-blank line with a placeholder comment |
| `off_task` | rename the first unchanged name that no added line uses |
| `needless_layer` | add a wrapper that forwards the parameters of a function in the same file, and call it from the first added call |
| `narration` | add a comment above the first added statement that repeats it |
| `unguarded_state` | delete one unchanged lock or channel line from a concurrent code edit |
| `wasted_work` | wrap the first added call in a three-pass loop |
| `unsafe_input` | add a literal credential line to a code or config edit |
| `unclear_contract` | rename the first added function to `handle` |
| `off_token` | replace the first added CSS variable with a literal colour |
| `layout_motion` | add `transition: all 0.3s;` to the first added CSS block |
| `incomplete` | restore the first file named by the turn subject to its pre-image, retaining its path and position |

The defect pools were counted in one pass over the 1,640 non-merge commits of the history, with no quota cutoff. A defect rule takes one file of a single-file sound commit, and `incomplete` takes a turn. The pool is the number of commits where the rule finds a target.

The incomplete rule MUST retain every file and its pre-image in the original order. It MUST set the named file's post-image to its pre-image.
The rule MUST recompute that file's change kind and MUST NOT mutate the original inputs. Remaining files MUST still add work for the case to qualify.
The recorded pool counts await a census with this corrected rule.

| Question | Plan quota | Pool | Used |
|---|---|---|---|
| `broken_syntax` | 30 | 279 | 30 |
| `lost_code` | 30 | 82 | 30 |
| `wrong_logic` | 30 | 196 | 30 |
| `placeholder` | 30 | 563 | 30 |
| `off_task` | 30 | 89 | 30 |
| `needless_layer` | 30 | 7 | 7 |
| `narration` | 30 | 500 | 30 |
| `unguarded_state` | 30 | 2 | 2 |
| `wasted_work` | 30 | 23 | 23 |
| `unsafe_input` | 30 | 536 | 30 |
| `unclear_contract` | 30 | 19 | 19 |
| `off_token` | 30 | 0 | 0 |
| `layout_motion` | 30 | 2 | 2 |
| `incomplete` | 30 | Not measured with the corrected rule | 30 |

`off_token` yields no case under this rule, because the history has no single-file interface edit that adds a `var(--...)` line. The held-out split holds 25 cases of the six short groups: 3 `needless_layer`, 1 `unguarded_state`, 11 `wasted_work`, 9 `unclear_contract` and 1 `layout_motion`.

Counting one file of every multi-file sound commit as well would find more candidates. The counts are 89 for `needless_layer`, 38 for `unguarded_state`, 280 for `wasted_work`, 283 for `unclear_contract`, 11 for `layout_motion` and 3 for `off_token`. The rule keeps to single-file commits, so those commits are not cases.

The held-out run sends one request for each part of each held-out case. It needs `DALGONA_EDIT_REVIEW_EVAL=decisions:typesafe/jev-1.14` and `OPENROUTER_API_KEY`, and it skips without them.

The evaluator MUST prepare every held-out request before any judge call or tally.
A turn input MUST have positive additions after masking and diff construction.
An ineligible turn input MUST fail preflight and return no measurement, not a service error or a skipped case.
Any other preparation failure MUST count as one errored unit and MUST send no request.
An errored unit MUST count toward the 5 percent limit.
An unchanged named file MUST stay in the corpus input and MUST NOT appear in the changed-lines request.
Preparation MUST build each request only once.

The sound flag rate counts each part of each sound case, because each part can speak alone. Precision, recall and true flags count each case once. Recall is absent when no held-out case is a positive for the question. A case flags a question when any of its parts flags it.

A unit whose parts all fail is one error. The test fails when more than 5 percent of held-out units error. The test logs, for each question, its true, false and missed flags, its precision and its recall, and the ids of the errored cases. The test logs the p95 latency of each part. A p95 above 4 seconds MUST set `reviewMaxParts` to 4 before the feature is on by default.

Corpus rebuilding MUST preserve measurements only when the corpus SHA-256, judge identifier, and threshold are identical.
A change to any identity field MUST reset `measured` to `null`.
The held-out evaluator MUST send turn cases through the production turn request.
It MUST send changed lines only in one bounded request.
Truncated turns MUST NOT speak.

A question MUST NOT speak until a measurement names it. A question names itself when its held-out precision is at least 0.9 and it has at least 5 held-out true flags.

Before the feature is on by default, every question MUST have held-out precision of at least 0.9 and recall of at least 0.5. The sound flag rate MUST be at most 0.02. A question whose precision or recall is absent fails the gate. The ratchet check MUST compare every question that `ratchet.json` records, and a question missing from the run fails the check.

No live run has been made. `OPENROUTER_API_KEY` was absent when the corpus was built. `ratchet.json` holds `measured: null`, so no speaker is measured. `reviewBuiltInSpeakers` derives from the `speaker` flag of each question in `reviewQuestions`, and every flag is unset.
