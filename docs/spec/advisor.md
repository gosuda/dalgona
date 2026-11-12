# Advisor

The advisor is an optional reviewer. It reads the transcript of a primary thread and sends short notes back into that thread. Both products, `dal` and `dalgona`, MUST support it. The advisor MUST NOT approve or reject actions. The advisor MUST NOT change primary thread state, except when it delivers a note.

## Configuration

`config.toml` and `settings.toml` MAY contain an `[advisor]` table and an `[advisors.<name>]` roster. Both products MUST accept both tables. `settings.toml` MUST override `config.toml` key by key. Unknown keys MUST fail `config.Load` with `<file>:<line>: unknown key "<key>"`.

| Key | Type | Default | Values |
| --- | --- | --- | --- |
| `advisor.enabled` | bool | `false` | `true` MUST turn the advisor on. No command-line flag sets it. |
| `advisor.review` | string | `"turn"` | `turn`, `agent-end` |
| `advisor.interval` | int | `1` | 1 or more |
| `advisor.sync` | string | `"off"` | `off`, `final`, `strict` |
| `advisor.continue_severity` | string | `"blocker"` | `nit`, `concern`, `blocker` |
| `advisor.immune_turns` | int | `3` | 0 or more |
| `advisor.max_notes` | int | `3` | 0 or more |
| `advisor.tools` | string list | `["read", "search"]` | Tool names. `[]` grants no investigative tool. |
| `advisor.subagents` | bool | `false` | `true` MUST advise subagents too. |

An `[advisors.<name>]` entry MAY set `model`, `instructions`, `review`, `interval`, `sync`, `tools`, `subagents`, and `enabled`. An omitted `review`, `interval`, `sync`, or `tools` MUST inherit the `[advisor]` value. An explicit `tools = []` MUST replace the inherited list. An omitted `subagents` MUST inherit the subagent decision of `[advisor]`. `enabled` MUST default to `true`. `model` and `instructions` MUST default to `""`.

When no `[advisors.<name>]` entry exists, the roster MUST contain exactly one advisor named `advisor` with inherited values. When one or more entries exist, the roster MUST contain only those entries.

An advisor runs only when `advisor.enabled` is `true` and its entry has `enabled = true`.

### Validation

`config.Load` MUST reject an invalid value with `<file>:<line>: <message>`. The line MUST point at the key. Each message MUST match byte for byte:

| Key | Message |
| --- | --- |
| `advisor.review` | `[advisor] review must be one of turn, agent-end` |
| `advisor.interval` | `[advisor] interval must be at least 1` |
| `advisor.sync` | `[advisor] sync must be one of off, final, strict` |
| `advisor.continue_severity` | `[advisor] continue_severity must be one of nit, concern, blocker` |
| `advisor.immune_turns` | `[advisor] immune_turns must not be negative` |
| `advisor.max_notes` | `[advisor] max_notes must not be negative` |
| `advisors.<name>` | `[advisors.<name>] name must be lowercase letters, digits, _ or -, 1-32 chars` |
| `advisors.<name>.model` | `[advisors.<name>] model must be smol, default, slow, advisor, or provider/model` |
| `advisors.<name>.review` | `[advisors.<name>] review must be one of turn, agent-end` |
| `advisors.<name>.interval` | `[advisors.<name>] interval must be at least 1` |
| `advisors.<name>.sync` | `[advisors.<name>] sync must be one of off, final, strict` |

A name MUST match `^[a-z][a-z0-9_-]{0,31}$`. When a name is invalid, the loader MUST report only the name error for that entry. Errors MUST come in this order: `[advisor]` keys, then entries sorted by name. Inside one table, the order MUST be `review`, `interval`, `sync`, then the table-specific keys.

### Model role

`[roles] advisor` names the advisor model. An empty entry `model` MUST mean the role `advisor`. The role `advisor` MUST resolve in this order:

1. `[roles] advisor`, when set.
2. `[roles] slow`, when set.
3. The `slow` preset of the thread model's provider, when the provider offers it.
4. The thread model.

When the preset is not offered, the resolver MUST log `roles.advisor preset <id> is not offered; using <model>`. The advisor request MUST ask for effort `medium`, clamped to the efforts that the model supports.

A provider with no static preset MUST use its first offered model as its `slow` preset.

## WATCHDOG.md

`WATCHDOG.md` holds guidance for the advisor only. The primary thread MUST NOT receive it.

`Watchdog(home, cwd)` MUST read candidates in this order:

1. `<home>/WATCHDOG.md`.
2. For each directory from the repository root down to `cwd`: `<dir>/WATCHDOG.md`, then `<dir>/.dalgona/WATCHDOG.md`.

The repository root is the nearest ancestor of `cwd`, or `cwd` itself, that contains a `.git` directory or file. When no such ancestor exists, step 2 MUST use only `cwd`. A path MUST appear at most once. The first occurrence MUST keep its position. A candidate that is missing, unreadable, or not a regular file MUST be skipped without error. A FIFO or a directory named `WATCHDOG.md` MUST NOT block discovery.

Each file MUST be wrapped as `<watchdog path="<path>">` + content + `</watchdog>`. The path MUST be HTML-escaped. The content MUST NOT be changed. Blocks MUST be joined with one blank line. When no file is read, the result MUST be the empty string.

## Advisor system prompt

`System(maxNotes, instructions, watchdog)` MUST return the embedded prompt with `{{max_notes}}` replaced by `maxNotes`. Non-empty trimmed `instructions` MUST follow as `<specialization>\n<instructions>\n</specialization>`. A non-empty `watchdog` MUST follow last. Parts MUST be joined with one blank line.

## Runtime

There MUST be one advisor runtime per pair of thread and advisor name. The runtime MUST be a child thread in the journal: `session.created` with `parent` and `advisor: <name>`. Its transcript, usage, and in-flight review MUST recover after a crash like any thread. The advisor MUST NOT be a peer. `thread/list` MUST NOT list it. Clients MUST NOT address it, except read-only through `thread/read`.

`Attach(ctx, host, thread, cfg)` MUST build one runtime for every roster entry of the thread. The runtime MUST create the advisor thread at its first review. An existing advisor thread MUST be resumed with the advisor tools and prompt. `Runtime.Close` MUST remove the boundary hook, stop the review loops, and close every advisor thread. A later `Attach` MUST then resume the thread with its own tools. When `Attach` fails, it MUST close every advisor thread that it opened.

The advisor model MUST be the first non-empty value of: the entry `model`, `[roles] advisor`, `[roles] slow`, the role `advisor`. The host role resolver MUST then resolve that value against the thread model.

### Review input

A review MUST receive the primary's new active-path events after the advisor cursor (a journal seq). The cursor MUST reset on compaction, revert, leaf move, and fork. After a reset, the next review MUST replay the bounded current active path. When the advisor is enabled mid-thread, the cursor MUST start at the current head. A fork without an advisor thread MUST start its cursor at 0.

The review text MUST be at most 64 KiB minus 1024 bytes, so that the journal keeps it inline. The runtime MUST keep the newest rendered events that fit. When older events do not fit, the text MUST start with `[<n> earlier events omitted]`. The newest event that does not fit MUST keep its first and last bytes, joined by `\n[<k> bytes omitted]\n`, when at least 64 bytes of budget remain.

The advisor thread MUST receive the review as one prompt: `<review seq="<head>">\n` + review text + `</review>\n`. `<head>` MUST be the primary journal head that the review covers. The runtime MUST submit the review prompt with `Input.Verbatim`. Keywords and `@path` mentions in the primary transcript MUST add no notice, no attachment, and no effort change. Each review prompt MUST request effort `medium`. After a restart, the cursor MUST be the `seq` of the last review prompt whose advisor turn completed. When no such prompt exists, the cursor MUST start at the current head. The last review prompt MUST then complete once. When it succeeds, the cursor MUST move to its `seq`.

`Render(events)` MUST produce the review text with these elements, one per line group:

| Event | Output |
| --- | --- |
| `user.message` | `<user>\n<text>\n</user>` |
| `assistant.message` reasoning | `<reasoning>\n<text>\n</reasoning>` |
| `assistant.message` text | `<assistant>\n<text>\n</assistant>` |
| `assistant.message` tool call | `<tool_call name="<name>" id="<id>">\n<args>\n</tool_call>` |
| `tool.completed` | `<tool_result id="<id>" ok="<bool>" exit="<code>">\n<output>\n</tool_result>`. `exit` MUST appear only when the event has an exit code. |
| `compaction` | `<summary>\n<summary text>\n</summary>` |
| `turn.completed` with status other than `completed` | `<turn_end status="<status>"><error></turn_end>` |

Every element MUST end with a newline. Text MUST escape `&`, `<`, and `>`. Attributes MUST be HTML-escaped. A blob reference MUST render as `[<bytes> bytes in blob <sha256>]`. Other event types MUST NOT render. `tool.started` MUST NOT render, because the assistant tool call already carries the arguments. Tool output MUST be the masked model-visible projection.

Delivered notes MUST NOT appear in review input. The `advisor.note` event MUST NOT render. Every complete `<advisory ...>...</advisory>` element in user text MUST be removed. The remaining user text segments MUST be trimmed and joined with one newline. A user message with no remaining text MUST NOT render. An unclosed `<advisory` MUST stay as escaped text.

### Cadence

A boundary is the point after a model step whose tool calls all completed (`turn`) or the point where the model stops calling tools (`agent-end`). The end of a failed or interrupted turn MUST also be an `agent-end` boundary, but the primary MUST NOT wait there. With `review = "turn"`, every boundary MUST be eligible. With `review = "agent-end"`, only `agent-end` boundaries MUST be eligible. A boundary of a continuation turn MUST NOT be eligible. Only an advisor `continue` delivery while idle MUST start a continuation turn. Input that arrives while the `turn_end` hook runs MUST start a normal turn when the turn completed and auto-continue is on. The advisor MUST review at every `interval`-th eligible boundary. Events from skipped boundaries MUST go into the next review. A review MUST NOT start while the previous review of the same advisor runs. Reviews that are requested during a running review MUST merge into one review.

### Tools

The advisor MUST have the `advise` tool with arguments `{note, severity}`, where `severity` is `nit`, `concern`, or `blocker`. It MUST also have the tools in its `tools` list that the runtime can build: `read`, `search`, `exec`, and `web_fetch`. Other names MUST NOT give the advisor a tool. Those tools MUST run through the same approval gate and runner as primary tools, in the advisor's own snapshot store. An advisor thread has no approval client. It MUST NOT wait for a user decision. In `ask` mode, its calls fall back to the configured decider. When no decider is configured, its calls decline. A call to a tool that is not granted MUST return `Tool <name> not found`.

The `advise` result MUST be exactly one word: the guard verdict, or `discarded` when the review output is unsafe (see Failures). An accepted `blocker` MUST go to the primary before the tool returns. An accepted `nit` or `concern` MUST stay pending until the review ends. At the end of a completed review, the runtime MUST deliver the pending notes in order. The runtime MUST compute them from the advisor journal: the accepted `advise` results of this review, applied in order through a fresh guard. A pending note that a later admission names in `Displaced` MUST be dropped. Because `Thread.Deliver` journals each (advisor, review_seq, note) once, a repeated delivery after a crash MUST NOT add an event.

An `advise` call whose arguments exceed 64 KiB MUST fail with the error `advise: note is too long`.

### Emission guard

Each advisor MUST have its own guard. `Normalize(note)` MUST apply NFKC, then lowercase. It MUST then replace each run of characters that are not Unicode letters or numbers with one space. It MUST then trim the ends.

`Guard.Admit(note, severity)` MUST return one verdict. The tool result MUST state it as `accepted`, `duplicate`, `noise`, or `budget`.

- `noise`: the normalized note is empty.
- `duplicate`: since the last reset, the guard accepted the same normalized note at the same or a higher severity. A re-raise at a strictly higher severity MUST NOT be a duplicate.
- `budget`: the note is not a `blocker`, the current review already holds `max_notes` non-blocker notes, and no displacement applies.
- `accepted`: otherwise.

A rejected note MUST NOT change guard state. A `blocker` MUST NOT use a budget slot.

Displacement: when the budget is full, a non-blocker note MAY replace the lowest-severity pending note of the current review. It MAY do so only when its severity is strictly higher. The admission MUST name the replaced note in `Displaced`. The caller MUST drop that pending note before it queues the accepted note. A re-raise of a pending note from the same review at a higher severity MUST name that note in `Displaced`. A pending note that a `blocker` re-raise replaces MUST free its slot. `Guard.Delivered(key)` MUST mark a note as delivered. A delivered note MUST keep its slot and MUST NOT be displaced. Notes from an earlier review MUST NOT be displaced.

`Guard.BeginReview()` MUST clear the budget slots and MUST keep the duplicate history. `Guard.Reset()` MUST clear both. The runtime MUST call `Reset` when the cursor resets. `Admit` MUST be safe for concurrent calls.

### Delivery

The primary MUST journal each accepted note as `advisor.note {advisor, severity, note, review_seq, delivery}`. The model MUST receive the note in the next user-role content as `<advisory advisor="<name>" severity="<severity>">` + escaped note + `</advisory>`. The note text MUST escape `&`, `<`, `>`, `"`, and `'`.

`Route(state, severity, policy)` MUST choose `delivery`:

- `steer`: the primary runs, and the note interrupts the current turn.
- `aside`: the note is shown and queued for the next turn without an interrupt.
- `continue`: the primary is idle, and the runtime starts one continuation turn.

The immune window is active when a concern or blocker steer was delivered and fewer than `immune_turns` boundaries have passed since then. Inside the window, a `nit` or `concern` MUST be `aside`. A `blocker` MUST ignore the window.

The primary is running when `Status.Busy` is true. It is idle when it is not busy and auto-continue is on. It is interrupted when it is not busy and auto-continue is off.

| Primary | Severity | Outside window | Inside window |
| --- | --- | --- | --- |
| running | nit | aside | aside |
| running | concern | steer | aside |
| running | blocker | steer | steer |
| idle | below `continue_severity` | aside | aside |
| idle | nit or concern, at or above `continue_severity` | continue | aside |
| idle | blocker | continue | continue |
| interrupted | any | aside | aside |

An idle primary MUST start at most one continuation per batch of notes. After that continuation starts, more notes from the batch MUST be `aside`. A continuation MUST NOT schedule another review. A user interrupt (Esc, `turn/interrupt`) MUST put the primary in the interrupted state until the next user input. In that state, auto-continue MUST be off.

### Sync

`sync = "off"` MUST NOT make the primary wait. `sync = "final"` MUST make the primary wait at each `agent-end` boundary until every review requested so far has ended, before it reports the turn completed. `sync = "strict"` MUST also make the primary wait at every boundary that requested a review. A wait MUST end when the review ends with any outcome. A wait MUST also end when the advisor halts or is disabled. A wait MUST also end when the primary turn is interrupted or the runtime closes.

### Failures

Reviews MUST use the shared retry rules. A review fails when the advisor turn does not complete, when its output is unsafe, or when a delivery fails. After 3 failed reviews in a row, the advisor MUST stop with status `halted`. It MUST stay halted until `Runtime.Reset` (`/advisor reset`) or a restart. A successful review MUST reset the count. A failed review MUST NOT move the cursor.

The output of a review is unsafe when an assistant text of the review contains a destructive shell directive. A directive that the review input also contains does not make the output unsafe. The rule set MUST come from `internal/agent/rules`: `rules.Check` with the rules `recursive-delete`, `git-destructive`, `publish`, and `deploy`. Candidates MUST be each text line, with backticks removed and a leading `$` removed, and each backtick span. An assistant text stored as a blob MUST count as unsafe. A `blocker` MUST be delivered only when the review output up to its call is safe and the advisor is enabled. Otherwise its result MUST be `discarded`. An unsafe review MUST deliver none of its pending notes. A blocker that was delivered before later unsafe output MUST stay delivered. When a live blocker delivery fails, the guard MUST forget that admission, so that a re-raise can be accepted.

### Protocol

The server MUST send the notification `dalgona/advisor/note {threadId, advisor, severity, note, delivery}` for each delivered note. Each note MUST also appear as a `dynamicToolCall` item named `advisor`.

- `dalgona/advisor/status {threadId}` MUST return `[{name, model, status, notes, usage}]`. `status` MUST be `idle`, `reviewing`, `halted`, or `disabled`.
- `dalgona/advisor/set {threadId, enabled}` MUST turn the advisor on or off for the thread.
- `dalgona/advisor/reset {threadId}` MUST clear the halted state and the failure count of the thread's advisors. The `status`, `set`, and `reset` methods MUST attach the thread's advisor runtime when it is not attached.

The server MUST serve these methods from the thread runtime. `Runtime.Status()` MUST return one entry per roster entry, sorted by name. `notes` MUST count the `advisor.note` events of the advisor on the primary active path after its last compaction. `usage` MUST sum the usage of the assistant messages on the advisor thread active path after its last compaction. `Runtime.SetEnabled(enabled)` MUST turn every roster entry on or off. An entry with `enabled = false` MUST stay `disabled`. Turning an advisor off MUST interrupt its running review, and that review MUST deliver no more notes and MUST NOT count as a failure. Turning it on MUST start the cursor at the current head.

The `usage` object MUST contain exactly `inputTokens` and `outputTokens`.

The TUI MUST render a note as a dim card. For the advisor named `advisor`, the card reads `advisor · <severity>: <note>`. For any other advisor, the card reads `advisor <name> · <severity>: <note>`. `/advisor` MUST open a panel that lists the advisors. Enter in that panel MUST toggle the highlighted advisor. `/advisor status` MUST list the advisors.

### Subagents

A subagent MUST run without an advisor, unless the `task` call, `[advisor] subagents = true`, or the advisor entry opts in. `Attach` MUST start the runtime of a thread with a `parent` as disabled for every advisor that does not apply. An entry with `subagents = true` MUST advise every subagent thread, also when the global key is `false` and the `task` call did not opt in. An entry with `subagents = false` MUST never advise a subagent thread, also when the global key is `true` or the `task` call opted in. An entry with omitted `subagents` MUST advise a subagent exactly when the global rule opts in. A `task` call opts in when its `advisor` argument is `true`. The key MUST NOT change behavior on a top-level thread. `[advisor] enabled = false` MUST turn every advisor off, and an entry with `enabled = false` MUST stay off.
