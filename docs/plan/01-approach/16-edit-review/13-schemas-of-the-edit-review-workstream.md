#### Schemas of the edit-review workstream

Config (TOML): the table is `[edit_review]`, dalgona only. It has two keys. `enabled` is a boolean that defaults to `false`. `feedback` is an array of question ids. When `feedback` is absent, the built-in speakers apply. The layers merge, so `settings.toml` overrides `config.toml`. dal rejects the table with `config.toml:<line>: edit_review is a dalgona setting`. The judge model comes from `[approval_decider] decider` and that key's default.

```toml
[edit_review]
enabled = true
feedback = ["lost_code", "placeholder", "incomplete"]

[approval_decider]
decider = "decisions:typesafe/jev-1.14"
```

Go host field: `HostConfig.EditJudge ClientFactory`, where `ClientFactory` is `func(ctx context.Context, threadID string) (decide.Client, string, error)` (`internal/agent/decider.go:63`). A nil value turns review off. The field serves both units. App: `EditJudge: judges.editJudge()` and `EditFeedback: cfg.EditReview.Feedback` in the `agent.HostConfig` literal at `internal/app/app.go:91`.

Judge state for the edit unit (one edit-class call, `patch`, `apply_patch` or `rewrite`, sent as one or more parts, see Parts below):

| Field | Type | Rule |
|---|---|---|
| `task` | string | The latest user message on the active path, masked in the journal, cut to `taskLimit`, which is 2000 bytes (`internal/agent/gate.go:20`). Left out when empty. |
| `tool` | string | `patch`, `apply_patch` or `rewrite` |
| `files` | array | The files with a hunk in this part, in `tool.started` order. A file with no hunk is one unit with an empty `diff`, placed by the packing rule |
| `files[].path` | string | Masked |
| `files[].kind` | enum | `code`, `interface`, `config`, or `text`, from the file kind table below |
| `files[].change` | enum | `add` when `PreSHA` is empty, `delete` when `PostSHA` is empty, `update` otherwise |
| `files[].added`, `files[].removed` | int ≥ 0 | Lines from `textdiff.Stats` of the whole file, the same in every part that holds one of its hunks |
| `files[].diff` | string | `textdiff.Unified` of the masked pre-image and the masked post-image with 3 context lines (`udiff.DefaultContextLines`, `github.com/aymanbagabas/go-udiff@v0.4.1`, `unified.go:17`). It holds the two header lines and the hunks of this part, in order. `[binary file]` for a binary file. A file whose masked side is above `textdiff.Limit` is not in `files`. The sides are the masked bytes. |
| `files[].truncated` | bool | True when this part does not hold every hunk of the file, or holds a piece of a split hunk |
| `part` | object | `{index, of}`, both 1-based: this request is part `index` of the `of` parts that the unit sends. |
| `omitted_hunks` | int ≥ 0 | Units (a hunk, a binary file, or a file with no hunk) that have no piece in a sent part, because the unit reached `reviewMaxParts` (8). The same count is in every part of the unit. |

Judge state for the turn unit (the whole turn, judged once):

| Field | Type | Rule |
|---|---|---|
| `task` | string | The same rule as the edit unit, read when the turn review runs. The turn review does not run when `task` is empty, so this field is never empty. |
| `files` | array | Every path of the turn, in the order of its first counted edit. A file with a side above `textdiff.Limit` is left out. Each entry is the cumulative change of one path. A path whose pre-image and post-image are equal is left out. |
| `files[].path` | string | Masked |
| `files[].kind` | enum | As in the edit unit |
| `files[].change` | enum | `add` when the turn's pre-image is absent, `delete` when the post-image is absent, `update` otherwise |
| `files[].added`, `files[].removed` | int ≥ 0 | Lines from `textdiff.Stats` of the cumulative diff |
| `files[].diff` | string | `textdiff.Unified` of the cumulative pre-image and the cumulative post-image with 0 context lines, so that only changed lines are sent, cut by the turn size rule below. The pre-image is the `PreBlob` of the path's first counted change (Step 4a). The post-image is the masked post-image blob of the path's last edit. The binary rule is that of the edit unit. |
| `files[].truncated` | bool | As in the edit unit |


- Size. Every size in this section counts bytes after JSON escaping, and every limit below counts the same way. A part's body is its `state` and its `questions`, as `decide.RequestSize` measures it, which is the body without the `model` key. The decisions adapter posts the same body with a `model` member, a few dozen bytes more. Each body is at most `reviewRequestLimit`, which is 30000 bytes. A task cannot exceed the limit alone: `task` is cut to 2000 bytes, and JSON escaping expands 2000 bytes to at most 12000 bytes. The OpenRouter guide says that requests above about 32,000 bytes of text run in long-context mode, and it does not say whether questions count toward that size, so 30000 bytes stays under it either way (https://openrouter.ai/docs/guides/community/jev.md).
- Parts. A part is one request. The unit of packing is the hunk: the text of one file's unified diff from one line that starts with `@@ ` to the next such line. The two header lines `---` and `+++` belong to the file entry. A binary file is one unit, and a file with no hunk is one unit with an empty `diff`.
- Packing. Walk the files in call order, and the hunks of each file in order. Add the next unit to the current part while the body of that part is at most `reviewRequestLimit`. The body is the state plus the questions that the file kinds of the part need. Adding the first unit of a file to a part also adds that file's entry and header lines to the body. Otherwise close the part and start a new part with that unit. A part is never refilled. The rule is deterministic.
- Oversized unit. A unit that alone does not fit an empty part is split at a line boundary. The room is the largest prefix of the unit that fits in a part that holds only the unit's file entry. Within the room, cut at the last blank line whose next line starts at column 0 after the diff marker; else at the last blank line within the room; else at the last line end within the room. Each piece gets a hunk header with recomputed start numbers and counts.
- Part limit. A unit that needs more than `reviewMaxParts` (8) parts sends the first 8 parts. `omitted_hunks` counts the units that are not sent (Step 4), where a unit is a hunk, a binary file, or a file with no hunk.
- Turn size. The turn unit is one request. When its body is above `reviewRequestLimit`, each file's diff is cut to an equal share of the room. The room is `reviewRequestLimit` minus the body with every `diff` empty. Files go in call order. Each file takes the room left divided by the number of files not yet decided. A file whose diff fits its share keeps all of it, and the share it does not use stays in the room for the files after it. A file whose diff does not fit is cut, with the truncation marker below, to fit its share. If the body with every `diff` empty is still above `reviewRequestLimit`, the turn unit journals the `error` verdict with the text `review: turn state exceeds the request limit`, and sends no request.
- Truncation marker: cut at the last `\n` that keeps the kept text and the marker within the limit, then append `[truncated: <n> more bytes of this diff]\n`. The marker counts toward the body size, and `<n>` is the number of bytes cut.
- Masking: paths and edit-unit post-images are masked through `Host.mask`. Pre-images and turn-unit post-images come already masked from the blob store. For an edit unit, the size, binary and `textdiff.Limit` checks of each file run on the masked bytes, because the judge sees those bytes. `loadReviewFile` masks the post-image before it classifies it. The task comes already masked from the journal. `requireMasked` then refuses the encoded state of each part and of the turn unit if masking it again would change it.
- Pre-filter: the edit unit is skipped when the sum of `added` over every file of the call is 0. The turn unit is skipped when no edit unit of the turn was judged, or when its cumulative diff adds no line.

File kinds. The kind comes from the final extension of the path, as `filepath.Ext` returns it. Each path matches one row.

| Kind | Extensions |
|---|---|
| `interface` | `.css`, `.scss`, `.sass`, `.less`, `.svelte`, `.vue`, `.jsx`, `.tsx`, `.html`, `.htm` |
| `config` | `.toml`, `.yaml`, `.yml`, `.json`, `.ini`, `.env`, `.lock` |
| `text` | `.md`, `.mdx`, `.txt`, `.rst` |
| `code` | every other extension, and a path with no extension |

Concurrency pattern. A question with this extra condition is asked only when an added line matches the pattern: `\bgo\s+\w|\bchan\b|\bsync\.|\batomic\.|\bselect\s*\{|Mutex|WaitGroup|\basync\b|\bawait\b|Promise|\bWorker\b|\bspawn\b|Thread|threading|asyncio|tokio|\bArc<|setTimeout|setInterval`.

Judge state example, edit unit, part 1 of 2, with three files so that every edit question is asked:

```json
{"task":"Remove the unused retry helper","tool":"apply_patch","files":[{"path":"internal/model/retry.go","kind":"code","change":"update","added":1,"removed":14,"diff":"--- a/internal/model/retry.go\n+++ b/internal/model/retry.go\n@@ -40,17 +40,4 @@\n ...\n+\tgo r.flush(ctx)\n","truncated":false},{"path":"web/src/lib/Retry.svelte","kind":"interface","change":"update","added":1,"removed":0,"diff":"--- a/web/src/lib/Retry.svelte\n+++ b/web/src/lib/Retry.svelte\n@@ -3,6 +3,7 @@\n ...\n+  color: #3366ff;\n","truncated":false},{"path":"config/retry.toml","kind":"config","change":"update","added":1,"removed":1,"diff":"--- a/config/retry.toml\n+++ b/config/retry.toml\n@@ -1,2 +1,2 @@\n ...\n+backoff = \"2s\"\n","truncated":false}],"part":{"index":1,"of":2},"omitted_hunks":0}
```

Questions. Each row is one `decide.Question` with `Type: decide.TypeNoul`, `Instructions` set to the sentence in the row, and `Criteria: map[string]string{"true": yes, "false": no}`. A question is asked for a part only when a file of a listed kind in that part adds lines and its extra condition holds. The `sound` row is asked in both units. The `incomplete` row is asked only in the turn unit. A part asks the questions for the files in that part, so a part may ask fewer questions than the unit.

| Dimension | Id | Instructions | `true` | `false` | Kinds | Extra condition |
|---|---|---|---|---|---|---|
| primary | `sound` | Do the added lines in `files[].diff` serve `task` and show no defect? Judge only the lines shown. A file with `truncated` set to true has lines you cannot see. | Every added line serves `task`, and no added line shows a defect. | An added line shows a defect, or the diff holds a change that `task` does not ask for. | all | none |
| correctness | `broken_syntax` | Do the added lines in `files[].diff` leave the file syntactically broken? Judge only the lines shown. A file with `truncated` set to true has lines you cannot see. | An added line opens a bracket, brace, parenthesis, string, or block that no shown line closes, or closes one that no shown line opens. | Every bracket, brace, parenthesis, string, and block touched by the added lines opens and closes within the shown lines. | code, interface, config | none |
| correctness | `lost_code` | Does the diff in `files[].diff` remove code whose behaviour no added line restores and that `task` does not ask to remove? Judge only the lines shown. | A removed line holds a statement, branch, check, function, or test whose behaviour no added line restores, and `task` does not ask for that removal. | Every removed line is rewritten by an added line, appears again elsewhere in the shown diff, or is removed because `task` asks for it. | all | none |
| correctness | `wrong_logic` | Does an added line in `files[].diff` contradict the lines shown around it, or drop an error value that it receives? Judge only the lines shown. | An added line holds a condition, bound, index or return value that contradicts the shown lines around it, or it drops an error value that the same line receives. | Every added condition, bound, index and return value agrees with the shown lines around it, and every received error value is checked or returned. | code, interface | none |
| completion | `placeholder` | Does an added line in `files[].diff` stand in for code that was not written? | An added line is a placeholder such as `TODO`, `...`, `pass`, `unimplemented`, an empty function body, or a comment saying existing code stays unchanged. | Every added function body holds working statements, and no added comment says that code was left out. | all | none |
| completion | `incomplete` | Does the diff in `files[].diff` leave a change that `task` names undone, or leave a placeholder where `task` needs working code? Judge only the lines shown. | `task` names a change, file, case or test that no hunk of the diff makes, or a hunk leaves a placeholder where `task` needs working code. | Every change that `task` names appears in the diff as working code. | turn unit only, any file kind | `task` is not empty |
| scope | `off_task` | Does the diff in `files[].diff` change code that `task` does not call for? | A hunk renames, reformats, moves, or rewrites code that neither `task` nor the change `task` asks for needs. | Every hunk changes code that `task` names or that the requested change needs. | all | `task` is not empty |
| complexity | `needless_layer` | Do the added lines in `files[].diff` add a function, type, option, or parameter that does no work of its own, or nest blocks too deeply? Judge only the lines shown. | The added lines introduce a function, type, option or parameter that the shown lines use in one place and that only passes its input on, or they nest blocks more than three levels deep. | Every added function, type, option and parameter does work of its own or has more than one user in the shown lines, and no added block nests more than three levels deep. | code, interface | none |
| slop | `narration` | Do the added lines in `files[].diff` add comments that describe the change or repeat the line below, or add commented-out code or debug output? Judge only the lines shown. | An added comment repeats the line below it or describes the change itself, or an added line is commented-out code or leftover debug output. | Every added comment states something the code cannot say, and no added line is commented-out code or debug output. | code, interface | none |
| concurrency | `unguarded_state` | Do the added lines in `files[].diff` touch state that two goroutines, threads, or async tasks share, or start a task, without a guard in the lines shown? Judge only the lines shown. | An added line reads or writes a value that two goroutines, threads or async tasks share, and no shown line guards it; or an added line starts a task that no shown line stops, waits for or cancels. | Every shared value that the added lines touch has a lock, channel or atomic operation in the shown lines, and every started task has a shown stop, wait or cancel. | code, interface | an added line matches the concurrency pattern |
| performance | `wasted_work` | Do the added lines in `files[].diff` repeat work inside a loop, or read input into memory with no bound? Judge only the lines shown. | An added loop repeats a call, allocation, query or file read whose result is the same in every pass, or an added line reads a whole input into memory with no bound. | Every call inside an added loop depends on the loop's own value, and every added read has a bound in the shown lines. | code, interface | none |
| security | `unsafe_input` | Do the added lines in `files[].diff` hold a literal credential, or pass an outside value to a command, query, path, or markup without binding or escaping it? Judge only the lines shown. | An added line holds a literal key, token or password, or it builds a command, query, path or markup from an outside value without the binding or escaping that the shown lines use elsewhere. | No added line holds a literal credential, and every outside value that reaches a command, query, path or markup is bound or escaped in the shown lines. | code, interface, config | none |
| design | `unclear_contract` | Do the added lines in `files[].diff` add an exported function that switches behaviour on a boolean or untyped value, or a name that does not say what the code does? Judge only the lines shown. | An added exported function takes a boolean or untyped value that selects between behaviours, or an added name (`helper`, `manager`, `util`, `data`, `handle`, `process`) does not say what the code does. | Every added exported function has one behaviour per name, and every added name says what the code does. | code, interface | none |
| visual | `off_token` | Do the added styles in `files[].diff` set literal values where the shown lines use variables, or add a gradient or remove a focus outline? Judge only the lines shown. | An added style sets a literal colour, font, spacing or radius where the shown lines of that file use variables for that kind of value, or it adds a gradient, or it removes a focus outline. | Every added colour, font, spacing and radius uses a variable where the shown lines do, and no added style adds a gradient or removes a focus outline. | interface | none |
| layout and motion | `layout_motion` | Do the added styles in `files[].diff` fix a container's size in pixels or hide overflow with no narrow-screen rule, or add motion with no reduced-motion rule? Judge only the lines shown. | An added style fixes a container's width or height in pixels or hides overflowing text with no rule for narrow screens in the shown lines, or it adds an animation or transition with no reduced-motion rule, or it transitions `all`. | Every added size bends with its container or has a narrow-screen rule in the shown lines, and every added animation or transition names its properties and has a reduced-motion rule. | interface | none |

Labels and feedback sentences. Each speaking question shows its label in the terminal and browser summary, and its sentence in the feedback block. `sound` has neither: it never speaks.

- `broken_syntax`: label `broken syntax`; sentence "The added lines may leave a bracket, brace, string, or block unclosed."
- `lost_code`: label `removed code`; sentence "The edit may remove code whose behaviour no added line restores."
- `placeholder`: label `placeholder`; sentence "An added line may be a placeholder instead of working code."
- `off_task`: label `off-task change`; sentence "The edit may change code that the task does not ask for."
- `wrong_logic`: label `wrong logic`; sentence "An added condition, bound, index, return value, or error handling may contradict the lines around it."
- `needless_layer`: label `needless layer`; sentence "An added function, type, option, or parameter may only pass its input on, or a block may nest too deeply."
- `narration`: label `narration`; sentence "An added comment may repeat the line below it, or an added line may be commented-out code or debug output."
- `unguarded_state`: label `unguarded state`; sentence "An added line may use shared state or start a task without a lock, atomic operation, stop, or wait."
- `wasted_work`: label `wasted work`; sentence "An added loop may repeat work whose result does not change, or an added read may have no bound."
- `unsafe_input`: label `unsafe input`; sentence "An added line may hold a literal credential, or build a command, query, path, or markup from an outside value without binding or escaping it."
- `unclear_contract`: label `unclear contract`; sentence "An added exported function may choose its behaviour with a flag, or an added name may not say what the code does."
- `off_token`: label `off-token style`; sentence "An added style may use a literal colour, font, spacing, or radius, add a gradient, or remove a focus outline."
- `layout_motion`: label `layout or motion`; sentence "An added style may fix a size in pixels with no narrow-screen rule, or add motion with no reduced-motion rule."
- `incomplete`: label `incomplete`; sentence "The task may name a change that the diff does not make, or leave a placeholder where the task needs working code."

Request example, decisions adapter (`decide.Decisions.Ask` sends this body to the decisions endpoint). The `model` key is the id from `decisions:<id>`. The top-level keys and the keys of `questions` are written in sorted order, as `writeQuestions` writes them (`internal/decide/json.go:57`); the keys of `state` follow the judge-state table. Each question value is `{"type":"noul","instructions":"<row text>","criteria":{"false":"<false text>","true":"<true text>"}}`, with the row's text from the question table. The example leaves the `files` array and the question values empty, so that it stays short. Each part of a unit sends one request of this shape, with its own `part`:

```json
{"model":"typesafe/jev-1.14","state":{"task":"Remove the unused retry helper","tool":"apply_patch","files":[],"part":{"index":1,"of":2},"omitted_hunks":0},"questions":{"broken_syntax":{},"layout_motion":{},"lost_code":{},"narration":{},"needless_layer":{},"off_task":{},"off_token":{},"placeholder":{},"sound":{},"unclear_contract":{},"unguarded_state":{},"unsafe_input":{},"wasted_work":{},"wrong_logic":{}}}
```

Request example, llm adapter (`decide.LLM.Ask`). It sends the same body with no `model` key, as the single user message. The system prompt is `llmSystemPrompt` (`internal/decide/llm.go:15`).

Response example, decisions adapter. The adapter reads the `answers` object. A noul answer needs `"type":"noul"` and a `noul` number (`internal/decide/decisions.go:238`). This example is for the edit unit with every edit id:

```json
{"answers":{"broken_syntax":{"type":"noul","noul":0.02},"layout_motion":{"type":"noul","noul":0.04},"lost_code":{"type":"noul","noul":0.94},"narration":{"type":"noul","noul":0.05},"needless_layer":{"type":"noul","noul":0.03},"off_task":{"type":"noul","noul":0.11},"off_token":{"type":"noul","noul":0.02},"placeholder":{"type":"noul","noul":0.03},"sound":{"type":"noul","noul":0.31},"unclear_contract":{"type":"noul","noul":0.06},"unguarded_state":{"type":"noul","noul":0.12},"unsafe_input":{"type":"noul","noul":0.01},"wasted_work":{"type":"noul","noul":0.04},"wrong_logic":{"type":"noul","noul":0.62}}}
```

Response example, llm adapter. It is one JSON object that holds every question id as a key and no other key (`internal/decide/llm.go:111-127`). An unexpected key fails the request (`internal/decide/llm.go:131-141`):

```json
{"broken_syntax":0.02,"layout_motion":0.04,"lost_code":0.94,"narration":0.05,"needless_layer":0.03,"off_task":0.11,"off_token":0.02,"placeholder":0.03,"sound":0.31,"unclear_contract":0.06,"unguarded_state":0.12,"unsafe_input":0.01,"wasted_work":0.04,"wrong_logic":0.62}
```

The turn unit asks two ids, `sound` and `incomplete`, and uses the same two response shapes with those two keys.

Decision rule.
- Each returned score is read on its own. A missing answer, or a score outside 0 to 1, fails its part: that part is `error`, and no score of that part is recorded. A failed part adds no scores, and the unit is `error` only when every part fails.
- For each scored question other than `sound`: a score of `reviewFlagThreshold` (0.9) or more is a flag. A score from `reviewUncertainThreshold` (0.5) up to, but not including, 0.9 is uncertain. A lower score is clean for that question.
- `sound` never flags and is not counted as uncertain. Its score only gates speech.
- Each part is judged as the single request was: `reviewVerdict` reads the part's own scores. The verdict of a part is `error` on any failure of its request, including a timeout, a factory failure, a masking failure, a missing or out-of-range answer, and an HTTP 413 from the decision service (cross-phase decision 7). Otherwise a part is `flagged` when a question flags, `uncertain` when a question is uncertain, and `clean` in all other cases.
- The verdict of a unit is `error` only when every part fails. Otherwise it is `flagged` when an answered part flags, else `uncertain` when an answered part is uncertain or the unit is truncated, else `clean`. A truncated unit is never `clean`, because part of its diff was not read. The recorded `scores` hold, for each question, the highest score over the parts that asked it, and for `sound` the lowest. `flags` and `spoken` are the unions over the parts, in question order.
- A flag speaks only when its question is a speaker and the `sound` score of the same part is under `reviewSoundThreshold` (0.5).
- The candidates for the built-in speakers are `broken_syntax`, `lost_code`, `placeholder`, `off_task`, `narration`, and `incomplete`. None of them speaks until the measurement in Step 9 names it, so `reviewBuiltInSpeakers` stays empty until then. `[edit_review] feedback` replaces this set when the key is present. The built-in set `reviewBuiltInSpeakers` is the set that the speaker rule of Step 9 derives from `ratchet.json`. A change to the set comes with the ratchet update in the same commit. A question becomes a speaker when its held-out precision is 0.9 or more and it has at least 5 held-out true flags. A speaker whose held-out precision falls under 0.9 stops being a speaker.
- `spoken` is the set of speaking flags, in question order. A rebuilt request reads `spoken` from the journal, never the current speaker set.
- Edit unit feedback: `feedback` is true when `spoken` is not empty and fewer than `reviewFeedbackCap` (3) earlier `tool.judged` events in the same turn have `feedback` true. Otherwise `feedback` is false and `spoken` is absent.
- Turn unit feedback: `feedback` is true when `spoken` is not empty and `truncated` is false. A cut diff hides part of the turn, so a truncated turn unit speaks nothing. The turn unit runs at most once for a turn id, and a turn that is judged once is not judged again, even after an `error`.
- Timing. The edit unit's file reads, hashing, masking, diffing, and the building of the parts run in one judge goroutine. The goroutine sends the parts with at most `reviewPartWorkers` (4) requests at a time, all under one `reviewTimeout` context of 8 s. A part that exceeds it fails with the timeout text, and the unit is `error` only when every part fails. Each edit unit has one 8 s limit, so a turn with edits in several rounds can wait up to 8 s in each round. The turn unit is one request, and it is waited on for at most `reviewTimeout`.
- A unit whose parts all fail gives an `error` verdict, which never produces feedback.

Journal event `tool.judged`:

| Field | Type | Rule |
|---|---|---|
| `call_id`, `turn_id`, `by` | string | Required, not empty. `by` is `judge:<model>` or `judge`. |
| `verdict` | enum | `clean`, `uncertain`, `flagged`, `error` |
| `paths` | string[] | 1 to 64 entries, none empty, in the order of the call's files. The first 64 are listed. Masked. On an `error` caused by masking, the list is `[paths withheld: masking failed]`. |
| `scores` | object name to number | Each name is a question id that was answered. Each value is between 0 and 1. Required for `clean`, `uncertain`, and `flagged`. Absent for `error`. |
| `flags` | string[] | No repeats. Each name is a key of `scores`. In question order. Not empty if and only if the verdict is `flagged`. |
| `spoken` | string[] | No repeats. Each name is in `flags`. In question order. Not empty if and only if `feedback` is true. |
| `feedback` | bool | True only on `flagged` with a non-empty `spoken`, while the turn's cap allows. Left out when false. |
| `truncated` | bool | True when a hunk was omitted, a part failed, or a file was left out as `large`. Left out when false. |
| `parts` | int ≥ 1 | The number of requests sent for the unit. Left out when 1. |
| `failed_parts` | int ≥ 1 | The number of the unit's parts that failed. Left out when 0. At most `parts`. Equals `parts` when the verdict is `error` and a request was sent. |
| `error` | string | Required if and only if the verdict is `error`. At most 1 KiB. Masked. |
| `elapsed_ms` | int64 ≥ 0 | Required |

The invalid cases are listed in Step 2.

```json
{"type":"tool.judged","call_id":"call_7","turn_id":"turn-3","by":"judge:typesafe/jev-1.14","verdict":"flagged","paths":["internal/model/retry.go","web/src/lib/Retry.svelte","config/retry.toml"],"scores":{"broken_syntax":0.02,"layout_motion":0.04,"lost_code":0.94,"narration":0.05,"needless_layer":0.03,"off_task":0.11,"off_token":0.02,"placeholder":0.03,"sound":0.31,"unclear_contract":0.06,"unguarded_state":0.12,"unsafe_input":0.01,"wasted_work":0.04,"wrong_logic":0.62},"flags":["lost_code"],"spoken":["lost_code"],"feedback":true,"parts":2,"elapsed_ms":1840}
```

Journal event `turn.judged`:

| Field | Type | Rule |
|---|---|---|
| `turn_id`, `by` | string | Required, not empty. `by` is `judge:<model>` or `judge`. |
| `verdict` | enum | `clean`, `uncertain`, `flagged`, `error` |
| `paths` | string[] | 1 to 64 entries, none empty. The first 64 paths of the turn in order of first edit are listed. Masked, with the same exception as `tool.judged`. |
| `scores` | object name to number | Each name is `sound` or `incomplete`. Each value is between 0 and 1. Required for `clean`, `uncertain`, and `flagged`. Absent for `error`. |
| `flags` | string[] | No repeats. Each name is a key of `scores`. Only `incomplete` can appear, because `sound` never flags. Not empty if and only if the verdict is `flagged`. |
| `spoken` | string[] | Same rule as `tool.judged`. |
| `feedback` | bool | True when `spoken` is not empty. Left out when false. |
| `skipped` | string[] | Paths with no post-image blob. Each path is in `paths`. Left out when empty. |
| `truncated` | bool | True when a hunk was omitted (a diff cut to fit counts as one), a part failed, or a file was left out as `large`. Left out when false. |
| `error` | string | Required if and only if the verdict is `error`. At most 1 KiB. Masked. |
| `elapsed_ms` | int64 ≥ 0 | Required |

The invalid cases are listed in Step 2.

```json
{"type":"turn.judged","turn_id":"turn-3","by":"judge:typesafe/jev-1.14","verdict":"flagged","paths":["internal/model/retry.go","web/src/lib/Retry.svelte","config/retry.toml"],"scores":{"incomplete":0.93,"sound":0.31},"flags":["incomplete"],"spoken":["incomplete"],"feedback":true,"skipped":["config/retry.toml"],"elapsed_ms":2650}
```

Feedback block, edit unit (user role). The block is placed at its journal position, after the tool results of its round and after any user input journaled during the round. It is built only from journal values, so rebuilding a request gives the same bytes. Each `spoken` question adds one bullet with its sentence, in `spoken` order. `paths` is the list of the call's paths joined with ", ", with each path escaped by `html.EscapeString`. A clean verdict leaves the request exactly as it is with review off. The earlier prefix never changes, so the provider's prompt-cache prefix still matches, and the Anthropic cache marker lands on this last message (`internal/model/anthropic/anthropic.go:200-210`).

```text
<edit-review call="call_7" paths="internal/model/retry.go, web/src/lib/Retry.svelte, config/retry.toml">
An automatic review of this edit found possible problems:
- The edit may remove code whose behaviour no added line restores.
Check the changed lines. If a problem is real, fix it. If not, ignore this review.
</edit-review>
```

Feedback block, turn unit (user role). It follows the same placement rules, at the journal position of its `turn.judged`. It has one bullet for each spoken question, in `spoken` order.

```text
<turn-review turn="turn-3">
An automatic review of this turn found possible problems:
- The task may name a change that the diff does not make, or leave a placeholder where the task needs working code.
Check the changed files. If a problem is real, fix it. If not, ignore this review.
</turn-review>
```

Notification `dalgona/tool/judged`. Fields: `threadId`, `turnId`, `itemId` (equal to `call_id`), `verdict`, `summary`, `paths`. The server sends it for every verdict, live only. `thread/read` history does not carry it.

```json
{"threadId":"thr_1","turnId":"turn-3","itemId":"call_7","verdict":"flagged","summary":"Flagged: removed code","paths":["internal/model/retry.go","web/src/lib/Retry.svelte","config/retry.toml"]}
```

Notification `dalgona/turn/judged`. Fields: `threadId`, `turnId`, `verdict`, `summary`, `paths`. The server sends it for every verdict, live only.

```json
{"threadId":"thr_1","turnId":"turn-3","verdict":"flagged","summary":"Flagged: incomplete","paths":["internal/model/retry.go","web/src/lib/Retry.svelte","config/retry.toml"]}
```

Summary, for both notifications. `ReviewSummary` builds it from the event. A `clean` verdict gives `No problems found`. A `flagged` verdict gives `Flagged: <labels>`, with ` (not sent to the model)` added when nothing is spoken. An `uncertain` verdict gives `Uncertain: <labels>`, or, when no label applies, `Uncertain: part of the diff was not judged` for a truncated unit and `Uncertain: no label applies` for one that is not truncated. Edit-review step 6 holds the full table: one sentence for each verdict, label and truncation case. A truncated `flagged` or `uncertain` sentence ends with `; part of the diff was not judged`. An `error` verdict gives `Failed: <error cut to 120 bytes>`. `<labels>` are the labels of the flagged or uncertain questions in question order, joined with ", ".

Web types. `EditReview {verdict: "clean" | "uncertain" | "flagged" | "error"; summary: string}`. The `fileChange` item gets `review: EditReview | null`. `AppNotification` gets two variants: `toolJudged {threadId, itemId, review}` and `turnJudged {threadId, turnId, review}`.

Corpus line (`cases.jsonl`). `unit` is `edit` for the files of one tool call, or `turn` for the cumulative change of one or more files. `group` is one of 18 values. For a defect case it is the question id, one of the 14 ids. For a sound case it is `sound-edit` (a sound edit case), `sound-turn` (a sound turn case), `multipart` (a sound edit case that needs two or more parts), or `moved` (a sound moved-function case). A sound case is recognised by its group, and no second field marks it. `flags` is `[]` for every sound case, and a one-element list holding the question id for a defect case. A sound case counts against every question that is asked for its unit and file kinds.

```json
{"id":"lost_code-017","group":"lost_code","unit":"edit","split":"held_out","source":"3c4b6f94","task":"Add retry jitter","files":[{"path":"internal/model/retry.go","kind":"code","pre":"...","post":"..."}],"flags":["lost_code"]}
```

Ratchet (`ratchet.json`). The file is created with `measured` set to `null`. After an eval run with credentials, `measured` holds the shape below. It has one entry for each of the 14 ids that have a precision: the 13 edit questions and `incomplete`. `true_flags` counts held-out true flags. The speaker rule of Step 9 derives the speakers from these values.

```json
{"corpus_sha256":"<64 hex>","judge":"decisions:typesafe/jev-1.14","threshold":0.9,"measured":null}
```

Measured shape, after a run. Values are numbers in place of the placeholders:

```json
{"sound_flag_rate":"<number 0 to 1>","errors":"<integer>","questions":{"<question id>":{"precision":"<number 0 to 1, or null>","recall":"<number 0 to 1>","true_flags":"<integer>"}}}
```

