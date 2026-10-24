# Changelog

## Unreleased
- Add typed product settings to interactive TUI sessions: products declare
  allowlisted startup-only rows. Confirmed edits update `dal.toml` atomically
  with owner-only permissions and preserve comments and formatting; invalid
  edits are refused without modifying the file. The settings picker shows saved
  next-start values alongside running values when they differ, and remote
  connections refuse product setting edits.
- Pass the invoking product factory to `dalgon dev run` scenario
  executions, evaluating test scenarios with the product's own configuration
  kind, defaults, and extension builds.
- Scope scheme resolver identity: the host mints caller identity for the
  extension that registered the URI scheme, ensuring resolvers query records
  through their own capability-scoped services without inheriting caller identity
  or cell approval.
- Keep wire reply identifiers out of test timeout diagnostics.
- Apply the session's current approval policy to extension process runs,
  including after reopening a session.
- Give extension setup the same resolved default model route used by turns.
  Existing sessions keep their default when another session saves a new one.
- Restore branch previews, labels, file changes, and measured usage from the
  selected history. Keep queued openings busy until they start or stop.
- Keep scripted wire tests offline by serving model discovery on loopback.
- Build sandbox helpers from Cargo's reported artifact paths.
- Prevent PTY test harnesses from acquiring their children's controlling terminal.
- Add a non-publishing release preflight before package rehearsals. Resolve
  short and mixed-case crate names using Cargo's sparse-index path rules.
- Preserve cache-write prices when regenerating the model catalog.
- Count assistant replay payloads toward session-page byte limits so large
  replay records cannot bypass pagination.
- Clear stale session-lock owner records before publishing a new owner.
- Release abandoned grant questions and preserve revocations when a pending
  approval is saved.
- Redact captured working-directory paths in provider debug output.
- Describe the `agent` tool with a flat object schema for Anthropic
  requests. Keep action-specific argument checks in the tool.
- Report cancellation when `agent wait` observes a cancelled child.
- Stop retrying Google quota-exhaustion responses. Keep retrying temporary
  rate limits.
- Resolve family-only model routes through the single configured provider.
  Refuse ambiguous routes instead of choosing a provider implicitly.
- Stop unfinished process groups when their owner is dropped. A reaped leader's identity is consumed so a later drop cannot signal it again, and Unix wait paths observe the native leader before the group reap.
- Make live owned jobs observable in `list`, `find`, `status`, and counts without moving process capacity: owned rows join the running start-order stream while the session child limit still bounds process-backed children only.
- Bound completed Gemini CLI lines: a newline-terminated line longer than 1 MiB ends the stream with a protocol error before decoding, so the trailing result event cannot complete the turn; an exactly 1 MiB line is still accepted.
- Keep wire line-transport partial-read and close behavior: the bytes of a partly received line stay on the transport so a dropped read resumes the same line, and closing never waits out a stalled write while later writes fail closed.
- Reject sandbox write roots that overlap protected configuration or
  plugin paths, including automatic roots and symlinked parents.
- Accept `/exit` as an alias for `/quit` in terminal sessions.
- Offer browser and device-code sign-in for OpenAI Codex in `/login`.
  Browser sign-in also accepts a pasted localhost redirect URL when the
  browser cannot reach the callback listener.
- Avoid a second panic during terminal-loop cleanup by restoring the
  previous panic hook after unwinding.
- Preserve typed `j`, `k`, and spaces in picker filters. Ctrl+P and Ctrl+N
  move the selection with either an empty or a populated filter.
- Update the TLS certificate verifier and root bundles to address
  RUSTSEC-2026-0381, RUSTSEC-2026-0382, and RUSTSEC-2026-0383.
- Try the next Antigravity endpoint after a transient request failure before
  streaming starts. Stop immediately on errors that cannot be retried.
- Return an unavailable denial when a run approval expires without an answer.
  An explicit decline remains distinct. Neither outcome starts the process,
  and the next run can request approval.
- Keep a refused journal batch from widening live authority. A refused
  `ApproveForSession` answer takes its session allowance back, so the tool
  asks again instead of running on an allowance the journal never recorded,
  and a refused `SetApproval` puts the approval mode back to the one the
  journal records, so no policy snapshot grants under a mode no record backs.
- Verify WebSocket TLS with the platform certificate verifier instead of the
  bundled WebSocket root list. `wss` connections now trust the same system
  roots and chain checks as HTTPS requests; a verifier that cannot start
  fails the connect with a transport error that names the missing system CA
  certificates. Plain `ws` connections are unchanged.
- Refuse a piped API key that holds a line break or other control character.
  `login PROVIDER --api-key` forgave only the key's trailing line ending, so
  a multi-line input was saved as one key, reported as saved, and then failed
  at the first request with "failed to parse header value", an error that
  never names the key. It now exits 1 before storing anything and does not
  echo the input.
- Breaking Rust API: `McpClient::call` takes a `CancellationToken` as its
  third argument. It is the call's own cancellation, a child of the caller's
  turn token while that turn is live. Implementors stop waiting, tell the
  server the request is abandoned, and return `ServiceError::Cancelled` when
  it fires. A client that ignored it held a cancelled turn open until its own
  call deadline.
- Send stored text blobs in requests. A turn whose context held a text blob,
  such as a long pasted letter, failed with "blob ... was not read from the
  session store before the request"; the request builder now reads text
  blobs of any `text/*` media type back as inline text, as it already did
  for images. A blob that is absent, unreadable, damaged, recorded with a
  different length, or not valid UTF-8 fails the turn before any request is
  sent and names the blob.
- Bound the session store's blob read. A damaged or planted blob file
  larger than the 64 MiB blob limit was read whole before its digest was
  checked; it is now refused with the existing too-large error before any
  allocation.
- Route identity-named requests, such as the open judge's, through the
  catalog. A `provider/model` string that no registered model owns used to
  reach the HTTP provider as a synthetic route and fail with "an HTTP provider
  needs a concrete API model route", so history consolidation failed for every
  configured judge. A request that already names an API route keeps it.
- Keep a partial line when a wire read is cancelled. The serve loops drop a
  pending read whenever another event wins, and the bytes the read had taken
  were lost with it, which split the next frame. Closing a line transport now
  finishes the peer's stream after in-flight writes settle, even while other
  writer handles are alive.
- Stop printing the working directory in `EnvSnapshot`'s `Debug` output, as
  its documentation states.
- Fix a panic that unwinds the terminal client. It no longer aborts the process
  with "panic in a destructor during cleanup": the client used to swap the
  process panic hook back while the panicking thread unwound, and the standard
  library refuses that. The hook now goes in once for the process and always
  hands the panic to the hook that was there before it, after the terminal
  modes are restored. Each pass only registers its terminal state, so passes
  alive together in one process cannot overwrite each other's hook or leave a
  stale one holding a finished pass's terminal.
- Restore a reopened session's settings in its view. `restore_fold` rebuilt the
  leaf path and the context size from the replayed fold but kept the settings
  the host seeded, so a journaled `Model`, `Thinking`, `Approval`, or `Mode`
  record and the session name replayed into the fold without reaching
  `View.settings`, `SessionInfo.name`, or the approval a child session copies.
  `Session::settings_view` is now public and is the one projection behind every
  settings update and the restored view. `SettingsView.model` stays the route
  the session itself selected: a session that only follows the configured
  default shows `None` before and after a reopen, and after a live change of
  another setting, which used to publish the default as if it were selected.
  A selected route shows with its provider even when it equals the default.
  Journals written by an earlier build read as before, including a `model`
  record without a provider.
- API model routes name their provider. `ModelRoute::Api` gains an optional
  `provider`. It is left out of JSON and journal `model` records when absent,
  so every existing record and command reads and writes exactly as before. A
  route that names its provider resolves
  that provider's row and nothing else: not through a same-family provider
  that lists the same native id, and not through a registered synthetic model
  that spells the same id. When that provider is not configured, or serves
  another API family, `dal_provider::resolve_route` returns the typed
  `ResolveError::UnknownProvider` or `ProviderFamilyMismatch`; the turn then
  fails with that error's text (`model <provider>/<model> did not resolve:
  ...`) and no other provider is tried. The
  route survives `SetModel`, the journal, session views, reopen, fork, the
  next request, saved defaults, and restart. Routes that dal resolves itself
  (`/model PROVIDER/MODEL`, the pickers, `Host::models`, `default_route`, and
  the default model) now name their provider; `/model` and the mode lines
  print it instead of a family label. A route without a provider (an older
  record, a hand-built command, an id typed into the router) keeps the
  previous resolution: the bare id resolves first, and when it is unknown
  or ambiguous between same-family providers the route retries under the
  catalog's single configured owner for that family, including a lone
  custom provider and never guessing among several; with several Chat or
  Responses owners the route retries under the canonical `openai` provider
  when that row is configured, serves the route's family, and resolves the
  id (the provider named by the `openai-chat/` and `openai-responses/`
  references); every other case keeps the bare lookup error. Context
  measurements still carry over between a route without a provider and the
  same model with one. Rust callers: add `provider: None` to
  `ModelRoute::Api { .. }` literals and `..` to patterns; `ResolveError` and
  `EncodeError` gain variants; `ModelRoute` gains `provider`, `with_provider`,
  and `same_model`; `dal_agent::ext::command::CatalogView` gains a
  `providers` field, so a literal needs it (a command reads each configured
  provider's API family there); `dal_provider::Catalog` gains `providers()`.
- `models/list` rows add a typed `route` and an optional `reference`
  (`provider/model` for an API row whose provider is known) beside the
  unchanged `id`, `provider`, `name`, and `contextWindow`; `auth/status` is
  unchanged. A remote picker selects a row by its `route` for the session
  only, because a remote host never saves a default, and scopes it by
  `provider/model`. A row from a server that predates the members keeps its
  old selection by name, is not scoped, and the list is never refused for
  lacking them. The protocol version stays 1.
- Model rows carry the catalog provider that resolved them (`ModelInfo.provider`,
  absent in older payloads and read as empty). Synthetic rows report their
  model-id namespace. Provider-qualified model scoping can now use the row's
  own provider instead of rebuilding it from a display label.
- Add `Host::delete_session`: it refuses a session that is live in the host,
  leaves a tree whose lock another process holds untouched, and fails with
  not-found for an unknown id.
- Session listings carry `messageCount`: the number of entries on the
  session's visible leaf path, folded from the journal and cached in
  `info.json` beside it. It is omitted when the journal cannot be read, and a
  cache written before it existed is rescanned by a listing instead of read as
  zero (a name or id lookup, which never shows the count, still trusts it). The
  resume picker shows that count and says "messages unreadable" for a row it
  could not measure, where it used to print "0 messages" for every row.
- The resume picker opens the chosen session in that session's own workspace,
  and opens it before leaving the current one. A session that cannot open
  (held by another process, gone, or missing from the listing) changes
  nothing: the screen, the draft, and a running turn stay as they were, and
  the reason appears on the live screen. A turn is cancelled only once the
  replacement session is open. A live sub-session of the current session is
  refused the same way, before anything is cancelled or closed, because
  closing the current session would end it: `Host::open_replacing` makes that
  check for a caller that opens the successor first and closes after.
- `/resume NAME` and a direct resume find a session beyond the 200 rows a
  listing returns. `Host::find_session` resolves a full id, an exact name, or
  an id prefix across the host's workspaces with the store's own resolution
  and names the workspace the session lives in.
- An armed delete in the resume picker belongs to that picker opening: Ctrl+C,
  Esc, a move, a filter edit, or a paste drops it, and the `press d again`
  prompt is drawn from the picker holding the arming, so it is on screen
  exactly while that arming lasts and the next `/resume` needs a fresh
  first `d`. A delete whose listing refresh fails no longer leaves the
  deleted row on screen, and the resume picker shows its `d delete` hint.
- The quit confirmation answers to `y`, `Y`, `n`, and `N` again, with or
  without a reported Shift, and `n`, Esc, and Enter on the default focus close
  it keeping the draft. Uppercase `Y`, `A`, `N`, and `V` answer approval and
  confirm questions on terminals that do not report Shift.
- Suspending with Ctrl+Z and closing the external editor repaint the live
  block in full, so what the shell or the editor left on screen is cleared.
  After a resume from an ephemeral start, the exit line offers the resume
  token instead of saying nothing was saved.
- On Linux and Android, Ctrl+Z normally stops the process before the
  interface re-enters any terminal mode when the stop signal is unblocked
  with its default disposition in a non-orphaned foreground group. This
  is the normal case, not a guarantee. Other systems keep the previous
  best-effort
  behavior, where the stop can still arrive after a repaint.
- Remote terminals now learn the first model selection at once: the status row
  updates as soon as the selection is made, instead of only after a later
  refresh.
- The terminal status row prints `provider/model` for provider-qualified
  model routes. Long labels still truncate to fit the available width.
  All other routes print as before.
- The scoped-model picker lists a committed scope id the model list no longer
  names as its own checked row, so it stays until unchecked and unchecking
  every row clears the scope.
- `/mode`, `/thinking`, `/model`, and `/name` report what happened to the
  command. A command the host rejects before changing the session says it did
  not change anything, and a failed save says why it did not persist; neither
  prints "Saved as the default." or the resume hint. A rejected prompt from a
  skill or review command now reads `rejected: …` instead of `save failed: …`.
- Complete the native Ollama and Mistral conversations wires. New built-in
  providers `ollama`, `ollama-cloud`, and `mistral` need no `[providers]`
  entry. Ollama model discovery joins `GET /api/tags` to a `POST /api/show`
  probe per model (six at a time): `thinking` opens the reasoning levels,
  `vision` opens image input, a model without `tools` is sent none, and
  `<architecture>.context_length` sets the context window; a failed probe
  keeps the model's last cached row and a server without `/api/show` still
  lists its models. On Ollama Cloud `options.num_predict` is held to 65,536,
  DeepSeek V4 Pro and Flash carry that output limit, and replayed history drops
  assistant reasoning except for GLM 5.2; a self-hosted server now replays
  reasoning instead of stripping it. Mistral cards map `capabilities.reasoning`
  to `reasoning_effort`, a model with no reasoning card is sent no reasoning
  control, and prior reasoning replays only while reasoning is on. Signed thinking chunks are kept verbatim in arrival order and replayed as the full array ahead of the message text for an assistant message this family and model produced, only while reasoning is on and only when the stored shape and text still match; anything else replays as one text-only chunk.
- The automatic compaction notice at a tool-round boundary now reports the
  measured context size that triggered it (`Compacting context (threshold, 85
  percent of the window).`). It read the usage after the round's route was
  cleared, so a session whose requested model is a `provider/model` reference,
  such as the configured default, always reported `usage or context window is
  unavailable` even when measured usage drove the decision.
- Repair the guard's warning router for real exec results. The exec tool
  spills a long result to `<session dir>/jobs/<job id>.log` and ends its text
  with a `Full output: <path>` line; the guard now reads that log, takes the
  last such line as the tool's own so output that merely looks like a footer
  cannot name the file it reads, and surfaces the warnings the 4 KiB preview
  lost. The hook preview keeps that footer inside its 4096-byte bound, and
  `fs.read` serves a Builtin caller an absolute path under the session's own
  `jobs/` directory. Every other absolute path, and every absolute path from
  a non-Builtin caller, is still refused.
- Cancelling a turn while a `before_request` hook is held now ends the hook and
  the turn at once. The hook ran under the session's cancel token, so the turn
  waited out the hook deadline and then sent a provider request for the
  cancelled turn; the hook now runs under the turn's token and no request is
  sent.
- Add the `pi-messages` API family (`api = "pi-messages"`) and the built-in
  `radius` provider. Requests post `{model, context, options}` to
  `<base>/messages` and decode the gateway's assistant-message event stream
  into the neutral stream grammar, including thinking signatures that replay
  on the next turn. Radius signs in with an API key (`RADIUS_API_KEY`) and
  lists models from the gateway's `/v1/config`. Rust callers with exhaustive
  matches on `Family` must handle the new `PiMessages` variant.
- In GitHub Copilot device sign-in, submitting a blank enterprise domain
  defaults to `github.com`. Prompts for required OAuth authorization codes
  or redirect URLs continue to require non-empty input.
- Cancelling CLI device-code authorization with Ctrl+C during completion
  polling restores the terminal cleanly and cancels authorization without
  saving incomplete credentials.
- Prefer device authorization over an API key in terminal sign-in when the
  provider does not offer browser sign-in, matching the interactive CLI.
- Breaking Rust API: `LoginProgress::AskPaste` adds field `default: Option<String>`.
  Constructors and pattern matches must handle the new field: pass `None`
  when a non-empty submission is required, or `Some(fallback)` when an empty
  submission accepts a fallback value.
- Keep provider names separate from their state in `login status`, including
  long provider names and custom Bedrock routes.
- Save private-effect receipts on fresh persistent sessions before acknowledging
  them, without requiring an earlier user turn or sidecar write. Ephemeral
  sessions remain memory-only.
- Add Factory Droid device sign-in and GitLab Duo browser sign-in. Verify
  Factory sessions before storing new tokens. Read sign-in overrides from
  the environment captured at startup.
- Serialize Droid refresh commits with shutdown so cancelled refreshes cannot
  publish new credentials after shutdown.
- Add explicit `api-key` and `none` authentication styles. Reject credentials
  at request boundaries that cannot carry them.
- Replace each accepted Cursor replay segment once, without duplicating its
  assistant messages or tool results. Invalid replay data still uses ordinary
  history. Keep blob identifiers in their textual form when binding replay
  records to tool results.
- Decode Cursor repository-connector announcements so the adapter can reject
  their account-level effects instead of silently ignoring them.
- Harden Cursor request identity and replay guards: provider-ID checks cover streamed announcements and exec frames (a foreign streamed call emits nothing; a foreign exec call answers ToolNotFound), malformed present replay is refused instead of rebuilt, detail payloads with whitespace or unpadded base64 alphabets decode before validation, and request-scoped wire IDs keep colliding normalized calls distinct while stored replay IDs stay verbatim. Tool-name resolution and the approval probe flow are unchanged. The opening request and the context handshake set hosted web search false.
- Preserve valid JSON when marking Droid Responses tools as non-strict.
- Retain service cancellation receipts until service admission consumes them,
  so repeated cancellations do not strand `ask` or `run` capacity.
- Add `ArtifactFile::StatusTxt` (`status.txt`) to task isolation directories,
  saved atomically with file mode 0600 on Unix. Rust callers with exhaustive
  matches on `ArtifactFile` must handle the new variant.
- Estimate history image context from its stored token bill, not its encoded
  image size. Avoid unnecessary automatic compaction before the next response
  reports usage.
- Reject compaction before calling a compactor when its selected prefix has
  no live context left to replace.
- Measure repeated compaction against the context still in use. Count the
  earlier replacement when it is removed, and do not count messages that
  an earlier compaction already replaced.
- Send the resolved provider model for auxiliary requests selected through
  model aliases, including history consolidation requests.
- Restore stored plain-text blobs before sending requests, including after a
  session restart. Reject missing, corrupt, or invalid UTF-8 text before
  sending it to a provider.
- Preserve in-memory blobs needed by an ephemeral fork's restart prompt.
  Forks and clones keep their blob contents after the parent closes without
  creating durable session files.
- Keep the live terminal block on screen when a resize requires scrolling.
  Place the session-saved message below the final live block so it does not
  overwrite the cancellation notice.
- Reuse the input-hook model resolution for the first request and any
  compaction before it. An explicit model selection before the request
  still takes effect without running the input hooks again.
- Release completed tool cards from live terminal state. Keep their output
  and elapsed time in the transcript across follow-up turns. Clear unfinished
  live cards when their turn ends.
- Stop attaching skill letter images to a model that does not read images.
  The `letter` hook now stores no letter records for such a model, passes the
  input through as ordinary text, and says why in one `letter` notice per
  session. It does the same when the model does not resolve at input time.
  A stored letter set that no longer matches the loaded skills stays frozen:
  the hook adds no duplicate records. A frozen set is no longer redrawn on
  every input, and the hook reports a failed letter write with its cause in a
  `letter` notice.
- Render the `skills` prompt section as plain text when the letter font table
  fails to parse, instead of omitting the whole section. Skill names and
  descriptions stay visible; previously the section disappeared.
- Breaking Rust API: `InputEvent` gains `model: Option<ModelInfo>`, the model
  resolved for the input hooks from the session's selection or configured
  default. A later explicit model selection can change the first request's
  model. Set `model: None` at each construction site. Script hooks see the
  same payload as before.
- Breaking Rust API: `dal_ext::letter::handle_first_input` stays public but
  takes the whole `InputEvent` instead of bare user parts, because the
  capability gate needs the turn's resolved model. Callers without a resolved
  route pass `model: None` and always take the text-only path. The
  first-input hook is also internal to `letter::extension`; extension-driven
  turns should prefer adding that extension.
- Reject inconsistent request-resolution causes before writing a session journal.
  Refused records leave both stored bytes and the live journal unchanged.
  Older records without a cause remain readable.
- Return the catchable `unavailable` code from `state.*` in an ephemeral
  session, so `ctx.try_call` can degrade instead of failing the handler.
- Start an unnamed child when `agents.start` gets no `name`. One plugin can
  now run concurrent children and start a child again in the same workspace.
- Discard every line of a batch whose partial write could not be rolled back.
  The journal records the batch start, and the next open quarantines it.
- Report mailbox delivery only after its journal write succeeds. Failed
  writes return a service error and leave the live steer queue unchanged.
  Full queues refuse messages without writing them to the journal.
- Respect OAuth cancellation while waiting for the auth lock and recheck it
  immediately before the atomic store. Cancellation observed at either check
  leaves stored credentials unchanged.
- Sync the parent directory on POSIX systems when deleting the last stored
  credential, making the removal of the credentials file durable.
- Fix Git path confinement for attached long options and short `-qo` selectors
  without rejecting valid `-S` or `-G` string operands or empty options like
  `--pretty=`.
- Keep remote updates flowing when the server refuses an unsubscribe after
  a subscription is dropped. Surviving sessions reconnect from their last
  accepted cursor. Cleanup from an older connection leaves its replacement
  and pending replies intact.
- Fix the `steer` example waking a new turn while a turn is running.
  It now reads the Boolean value from the `turn.is_idle` reply.
- Keep ACP cancellation separate for numeric and string request identifiers.
  A completed request no longer removes a newer request's cancellation slot.
- Allow Git targets that do not yet exist when their resolved paths stay
  inside approved roots. Symlink escapes remain denied.
- Reject hook rewrites that turn parallel read calls into mutating calls.
- Reject oversized extension sidecar writes before changing the stored value.
- Settle cancelled scope handles while child startup is still pending.
  Dropping the scope aborts its owned startup tasks.
- Fix remote API-key sign-in reporting cancellation after the key was stored.
  Submitted sign-ins now await the server's result.
- Persist turn-ID reservations before acknowledging prompts, follow-ups, and wake
  allocations. Cancelled queued turns no longer leave invalid journal gaps or
  reuse identities after restart. Queued text remains ephemeral.
- Journal compatibility: new `turn_reserved` records use format 2. Other record
  kinds retain format 1, and existing journals need no conversion. Readers must
  support format 2 once a reservation is written. Rust consumers must handle
  the new `Record::TurnReserved` variant.
- Make terminal snapshots independent of temporary-path lengths. Remove the
  changelog test that rejected valid Unreleased entries.
- Update schema generation to schemars 1.2.3, session identifiers to uuid
  1.28.0, SHA-2 to sha2 0.11.1, and Windows IPC to interprocess 2.4.5.
  Update property tests to proptest 1.12.0 and scheduler tests to shuttle
  0.9.6. Retain blake3 1.8.2 because starlark 0.14.2 requires that exact version.
- Breaking Rust API: registration conflicts require both claimants. Set
  `Conflict.rival` to the competing `Claimant` at each construction site.
- Breaking Rust API: `JudgeOpen.session_route` is now optional. Pass
  `Some(route)` for a configured model and `None` for a model-less session.
  Required judging still refuses to open without a usable route.
- Breaking Rust API: `RecordSource.site` is now optional. Use `None` for a
  native rule and `Some(site)` for a rule registered from a script.
- Finish the previous turn's end and settled hooks before opening the next
  turn. Guard summaries now survive immediate follow-up prompts.
- Release session backends and services after close instead of retaining
  them through a strong-reference cycle.
- Reduce temporary memory used to scan small session journals by sizing read
  buffers to the file length. Recovery and record limits are unchanged.
- Accept hashline patches without a final line break. Preserve whether the
  final body line ends with a line break instead of rejecting the payload.
- Preserve question-opening and answer order through the session actor.
  Immediate controller answers no longer fail with "has no open question"
  because they overtake the corresponding question.
- Fix `patch` argument classification rejecting freeform payloads: the tool
  decoded every `{`-leading call as the structured `{"changes": ...}` schema,
  so with a freeform `edit_style` (dalgona's default `hashline`) every
  registered `{"input": ...}` call failed with "missing field changes" before
  the patch parser could refuse the payload in the dialect's name.
  Classification now accepts either registered envelope, the freeform `input`
  string or the structured `changes` array, matching what dispatch runs for any
  configured style, and keeps the single `patch: invalid input: ...` refusal
  for calls matching neither.
- Load every bundled rule-pack condition within the 4194304-byte matcher
  budget: a condition whose eager determinization does not fit now compiles
  to a lazily determinized DFA capped at the same per-condition budget with
  identical match semantics, instead of being skipped at startup with
  `expands past the 4194304-byte matcher budget`. This returns
  `git-commit-no-force-push` and `git-commit-no-placeholder-message` to the
  loaded rulebook. The `docs-contract-markers` conditions now ship as the
  two per-marker conditions with an equivalent `+`-free gap and load
  eagerly; the `dalgona://rules` page shows both conditions.
- Fix DOT digraph rendering: `digraph` sources now render instead of falling back to `invalid dot`, single-line graphs keep both nodes, and undirected `graph { A -- B }` with `strict digraph` render; nested or concatenated graphs and HTML labels still fall back.
- Fix setting changes dropping earlier history from the model context: `SetThinking`, `SetModel`, `SetApproval`, and `SetMode` now publish a tree delta for their journal entry, so the next request's context and the leaf view keep every turn before the change.
- Name both claimants when a second extension claims a slash command: `command "<name>" is already registered by plugin "<first>"; plugin "<second>" cannot register it`.
- Let a skill declare MCP servers without its extension injecting `mcp`.
  The mapped tools publish under `<skill>.<server>.<tool>` once the session
  approves the declared server set, and each call still passes the approval
  ladder; `ctx.mcp` stays restricted to extensions that inject `mcp`, so a
  handler that never injected it gains no service call. A server that lists
  one remote tool name twice now maps the first and drops the rest with a
  counted warning instead of failing publication.
- Enforce the exact rule byte budgets in load order: an always-apply body
  caps at 12288 bytes and the always-apply prompt section at 40960, a
  rulebook line caps at 4096 bytes (cut, with the note) and the whole
  rulebook listing at 10240. Rules past a budget fall to the rulebook with
  the budget note, or are skipped with the budget as the reason when they
  have no description. Front-matter descriptions cap at 120 bytes. A
  `rule_fired` journal record for a pack or plugin rule now carries the
  rule's `origin`. `rules.watch = false`, `[rules] disabled`, and a
  same-named user rule file now hold the compiled detector lanes (with the
  watch note for `watch = false`), and the `dalgona://rules` page lists the
  complete inventory: every rule with source file, scope, mode, repeat, pattern,
  and body, plus the budgets and the swap rules. `rule://<name>` now
  serves the body of a bundled pack rule instead of reporting it missing.
- Find deferred tools through Anthropic's native tool search on first-party
  Anthropic models from Opus, Sonnet, Fable, and Mythos 4.5 on. The request
  carries the BM25 search server tool and each deferred tool marked
  `defer_loading`; the search call and its `tool_reference` result replay
  untouched on the next turn. Other models keep the `tool_search` tool, and a
  request never carries both. Turn requests now send the prompt-cache key
  `<session>:<compactions>:<visibility>`; the last number rises once each time
  the tool list changes, such as when a deferred tool is promoted. Rust
  callers: `ModelToolSpec` gains a `defer_loading` field, omitted from its
  serialized form when false; set it to `false` in struct literals.
- Add skill invocation. `/skill:<name> [text]`, and the whole tokens `$<name>`,
  `$<plugin>:<name>`, or `/skill:<name>` in a prompt, add each named skill's
  body to the turn; one prompt may name several. `/skill <prefix>` lists the
  skills whose name contains the text and runs nothing unless the name is
  exact. The composer opens a skills-only completion list on a token that
  starts with `$`; `/` completion never lists `skill:` entries.
- Render diagram labels with font bytes captured once at terminal startup.
  A missing font falls back to source instead of silently dropping labels.
  Rust callers must pass `FontSnapshot` to the diagram render APIs and use
  `RenderCache::with_fonts`; `EnvFacts` now carries the snapshot.
- Pass D2's standard-input and standard-output arguments when rendering a
  diagram. The renderer now receives SVG instead of the command's help text.
- Install the compaction policy when a session opens. A resumed session can
  compact before its next prompt, including a retry after blob storage fails.
- Finish session-end hooks and release the opening session when its caller
  cancels startup. Host shutdown waits for this cleanup.
- Fix the questionnaire example when a question omits `options`. Text questions
  now run instead of failing on a missing record field.
- Expose validated MCP declaration types and validation errors at the `dal_core`
  crate root. Existing `dal_core::ext` imports remain valid.
- Use eight-column diagram cache ranges. Resizing across a range renders fresh
  art instead of reusing a render from a wider range.
- Check judge readiness at session startup without generating a response.
  Explicit `on` fails when no judge is ready; `auto` disables judged rules
  with a note. Keep `rules.judge` separate from the shared `judge.gate`.
  Judged matches close the provider stream before requesting a verdict.
  Confirmed violations retry with a durable reminder; rejected matches retry
  with a pass note instead. Errors and timeouts retry with a failure note.
  Cancellation prevents a late rule fire or retry.
- Add Rust-only `Services::model_ready` for route and credential readiness.
  It does not grant inference authority or add a Starlark operation.
- Fix the judged-rule pass notice appearing for a confirmed violation instead
  of a rejected match.
- Restore the recorded model route when opening extension setup for a resumed
  session. An explicit session model still takes precedence. Compaction keeps
  route identity; a latest response with unknown provenance uses configuration.
- Reject a stream completion before changing session state when too few journal
  entry IDs remain for the response, failed tool results, and queued input.
  Exact-fit responses remain valid, and repeated call IDs get one failed result.
- Preserve the model route in completed and partial assistant records, including
  its API family. Reopening a session keeps context measurements tied to the
  actual route, even when different route kinds share a model name.
  `EntryKind::Assistant` gains an optional `route` field. Existing journal records
  remain readable without invented route metadata or a format-version change.
- Open F1 keyboard help in a scrollable overlay. Esc and Ctrl+C close it without
  changing the draft. `/hotkeys` still writes the key list to the transcript.
  Growing an inline terminal clears the old live rows before moving them, so
  dismissed help does not remain on screen or enter scrollback.
- Restore direct `rustc` bootstrapping of `docs-gen` without a compiled dal
  library or generated manual input. Cargo and standalone builds use the same
  generator source.
- Match complete exact registry prerequisites across stable and prerelease
  versions, including nonzero major versions. Ignore build metadata while
  preserving case-sensitive prerelease matching.
- Reject missing, ranged, or incomplete exact versions for dalgona-to-dal
  prerequisites when `publish-crates.sh --check-registry-deps` runs, including
  dry runs. Pin each prerequisite to a complete version such as `=0.1.0`.
- Release the session journal when malformed plugin grants prevent startup.
  Correcting `grants.toml` now lets the same host reopen the session.
- Correct manual configuration types and defaults, guard syntax, session links,
  environment controls, rule behavior and locations, and fullscreen search guidance.
  Point SDK users to the runnable example and state the sidecar session restriction.
  Describe the actual stress-gate limits and export behavior.
- Reject measured context sizes from a different active or selected model route.
  Fall back to the retained-context estimate when the routes do not match.
- Include before-turn and named rule reminders in model context and context
  estimates. Preserve them through replay, branch selection, and compaction.
- Keep context estimates, measured usage, and compaction counts consistent when
  selecting or reopening a session branch. After compaction, estimates count the
  retained context instead of the replaced history and compactor metadata.
- Fix cargo-binstall's Windows binary paths to include the `.exe` suffix.
- Fix `login status` returning success when every stored credential is expired.
  A ready provider still makes the command succeed even if another provider has
  expired credentials.
- Restore exit code 4 for a release dry run with no publishable workspace members.
  A missing named product and an empty semver-check workspace still exit 2.
- Fix the unused startup-field warning when building `dalgon` without the `tui`
  feature. Terminal builds retain the same product identity.
- Move the repository to one root Cargo workspace with explicit package paths and
  local Dalgona-to-dal resolution. The dal manual now regenerates into its package
  from explicit public roots with `docs-gen`, without scanning the repository root.
- Build with the latest stable Rust toolchain instead of a declared minimum
  version. CI updates stable Rust and selects tools with `@latest`. GitHub Actions
  retain full commit pins, and cargo-dist records its exact generator version.
- Change MCP stdio declarations from `command` to a validated `launch` object
  with `kind = command`, `npx`, or `uvx`. This is a breaking declaration change;
  move an arbitrary argv list to `launch = { kind = "command", argv = [...] }`.
  Grant and tool previews show typed package/version/executable identity or
  HTTPS authority. They hide opaque command executables and operands, all
  environment names and values, URL paths and queries, and remote arguments.
  Grant identity still covers the complete declaration, not its preview.
- Keep cancelled scope handles terminal when upstream work finishes late. Scope cancellation also closes inference streams that have already opened.
- Show bidirectional controls as visible escape sequences in approval text. Reject control-bearing sign-in URLs before opening a browser or writing a terminal hyperlink.
- Fix extension file services rejecting paths outside the workspace and reporting path and I/O failures. Missing files still return no content. Path checks do not provide isolation from concurrent filesystem changes.
- Fix patch rename destination validation across all supported edit dialects. Revalidate the destination before commit and remove an installed destination if the apply fails. Path checks do not provide isolation from concurrent filesystem changes: a directory entry swapped for a link between validation and write can still redirect the rename.
- Add `auth/cancel`: `auth/login` reports a `loginId` while a sign-in attempt is pending, and `auth/cancel` aborts it by id, so a remote client no longer waits out a login timeout to stop an attempt.
- Name the sign-in attempt in `login_finished`: the host update now carries the attempt's `loginId` (`HostUpdate::LoginFinished::login`, `RemoteHostUpdate::LoginFinished::login_id`), and the remote terminal sign-in waits for its own attempt instead of the first update for the same provider, so a concurrent sign-in to that provider can no longer end it with the wrong outcome.
- Route `dalgon login`, `logout`, and `login status` through the shared `Host` sign-in operations, so the CLI publishes the same `LoginFinished` lifecycle and policy path as the terminal and RPC front ends; only the terminal input/progress loop stays CLI-owned. `StoredCredential` now also reports the OAuth expiry the status table prints.
- Replace terminal control characters (C0, DEL, C1) in provider-supplied model ids and display names at the catalog decode and cache boundaries, and escape them again when rendering the `dalgon models` table, so a hostile id carrying newline, CSI, or OSC sequences cannot reach stdout or persist through `models.json`.
- Cover the interactive CLI sign-in with binary-boundary PTY tests: `dalgon login` runs on a real pseudoterminal against a loopback OAuth server, proving progress output, stored credentials, Ctrl-C cancellation, and terminal restoration. The loopback endpoint override is test-support-only and never read by release builds.
- Require a serve token before `dalgon --public` binds, and name the bound address in the plain-HTTP warning it prints. Loopback serves stay unchanged.
- Fix a failed patch delete leaving earlier deletes missing: when a later delete in the same patch cannot move to its trash file, earlier deletes are restored so "Nothing was written" stays true.
- Fix `auth/logout` removing every stored credential when `provider` was present but not a string: an optional member of the wrong type, `null`, or `params` that is not an object is now refused with `-32602`, and only an omitted member selects the default. This covers every optional member of the dal RPC methods (`session/list`, `session/view`, `session/subscribe`, `docs/read`, `auth/login`) and the ACP `session/list` filters, which still accept `null`.
- Fix `session/submit` refusing `cancel_queued`, `set_mode`, `set_scoped_models`, `export`, and `reload_plugins` as unknown command types: the RPC layer kept its own list of 13 command tags while the decoder accepts 18, so those commands could not be sent. The decoder now owns the tag set, for commands and for answers. An unknown tag is still refused with `-32602`, and its text is now ``unknown `type` variant `<tag>` ``.
- Fix `session/submit` writing session exports outside the workspace: an `export` command whose path is absolute or climbs with `..` is now refused with `-32602` before it reaches the agent, and no file is written. Relative paths inside the workspace and the default target still export as before.
- Preserve approval durability and cancellation ordering: request-opened updates follow actor admission, failed journal emits stop dependent driver effects, dropped service approvals use an actor-owned resolution handoff, turn-scoped service runs stop their real processes, and session-level tool approvals replay after restart.
- Fix question dialogs taking consent from type-ahead: a dialog that opened while you were typing, such as a plugin grant question, treated the next typed letter as its answer, so the `a` in `/goal` allowed the grant for the session. Each request dialog now starts locked: for 500 ms after its first frame it ignores every key except Esc, and its hint row lists the answer keys (`y allow · a session · n deny · v view · esc denies`) only once they work. Dropped keys are not replayed, and the next queued request starts locked again. `dal_tui::dialog::DialogUi` gains `tick`, `armed`, and the `ARM_DELAY` constant; a host that drives it directly must call `tick` once per frame before keys can answer.
- Add `dal_provider::PROVIDERS`, one table of the built-in providers (`ProviderDef`: id, display name, family, base URL, transport, API-key header and environment variables, OAuth sign-in, and request header shape), and make it the only place that names a built-in provider. Sign-in methods now come from a row (`ProviderDef::methods`), `login_providers()` returns the rows sorted by id, `ProviderEntry` carries its row as `def`, `auth.json` decodes as a map of members keyed by provider id (a key outside the table is still refused, and every stored entry keeps its bytes), and the login flow, token refresh, and `dalgon login` read their endpoints, ports, scopes, and environment variables from the row. `OAuthProvider` is removed.
- Fix the inline transcript spacing: every settled block, such as your prompt or a reply, is now followed by exactly one blank row, so each turn looks the same and the live block starts directly under the last blank row. Before, a block that settled in its own frame left three blank rows above it, because the screen scrolled by the whole live block on every commit, while blocks that settled together left none. The live block also stays under the transcript when it shrinks, such as when the command popup closes: the rows it frees now go below the status row instead of between the transcript and the composer.
- Fix plugin grant questions raised outside a turn: a slash command, or a battery task that holds no turn, now asks the user once through the terminal's question dialog instead of being denied with no question. The approval is stored and journaled like any other. With no front end that can answer (print mode, listen-only clients), the question is refused at once with the existing denial text instead of waiting out its timeout.
- Fix the terminal freezing while a slash command waits: a `Run` command now executes beside the input loop, so a question the command raises, such as a plugin grant, shows and can be answered.
- Fix the first grant approval on a fresh install: the grants file and its data directory are created when the answer arrives, before any session exists.
- Fix the judge ledger `model` field: it names the configured judge model or the session's selected model identity. Readiness does not generate a provider reply or replace that identity.
- Report a context overflow on the turn-end hook: a turn that ends failed because the provider rejected the request for size, and compaction did not recover, now sets `overflowed` on the turn-end event and its script payload. The journal record is unchanged.
- Fix sign-in and sign-out in the terminal and over RPC: `/login` and `/logout` in the terminal now open a provider picker and run the sign-in (API key, browser, or device code) instead of printing a notice, and `auth/status`, `auth/login`, and `auth/logout` work over RPC v1. The terminal and RPC reach one provider login runner, `dal_provider::login`, through `Host::login` and `Host::logout`; the command line calls the same runner.
- Add the `agents.prompt` service operation: a parent can give an idle child one final prompt and interrupt it after a bounded grace period.
- Add a typed `refused` reply to `agents.start`, with the reason a child could not start (`max_depth`, `workspace_unresolved`, `workspace_outside_root`, or `model_unroutable`). The reason text now reaches the parent, the model, and the orchestration report instead of a bare `cancelled`.
- Persist child tool restrictions and inherited approval modes so a resumed child keeps the same trust boundary after a host restart.
- Add the fullscreen transcript viewport: PageUp and PageDown (Ctrl+B and Ctrl+F) page through the transcript, scrolling away from the live edge detaches the view and shows `following stopped · press end to jump to the latest` in the hint, and End returns to the latest entry. New content never moves a detached view. The inline Ctrl+T transcript overlay shares the same viewport renderer, leaves with Esc or q, and repaints the main screen exactly as it was.
- Add transcript search: Ctrl+Shift+F (F3 on terminals without kitty keyboard) opens a filter row over the viewport top with a live match count; Enter jumps to the latest match and Esc closes the row.
- Remove unused public TUI items: the `composer` module, `markdown::render_prose`, and `markdown::render_code_block`.
- Journal every approval answer: an answer to a tool-call, service-run, extension-ask, or plugin-grant question is now written to the session journal before it takes effect, the resolution broadcast follows that record, and a resumed session shows which question was answered, by whom, and how. Before, answers reached only the front ends that were watching and never survived a restart.
- Persist sidecars per extension with the documented file-name rule.
- Fix terminal typing during startup: keys written while the capability probe runs now reach the composer as they are typed, instead of appearing only after the probe window ends.
- Fix the terminal status context slot: from 70% of the context window it reads `ctx {n}% · rising` and from 90% `ctx {n}% · high`, with the word carrying a warning or error role.
- Fix the idle hint: kitty-keyboard terminals show `shift+enter newline`, other terminals keep `ctrl+j newline`, and entries fold by priority as the row narrows (navigation keys first, then help, then newline, then interrupt, then send) while state cues such as `following stopped` and `waiting for you` never fold away.
- Fix the working spinner: it now animates through braille frames while a turn runs; under `DAL_NO_MOTION` it stays frozen on its first frame.
- Fix the End key: in the composer it moves the caret to the line end again; in the fullscreen viewport and the transcript overlay it jumps to the latest entry, and the keyboard help lists it as `Jump to latest`.
- Fix `--connect` sessions: the status line hides the git branch, because a remote host's workspace path does not name a directory on this machine.
- Fix the terminal against its design contract: replies now render bold text, inline code, dashed lists, task boxes, and fenced code behind a rule; the status line shows the git branch after the path, the running job count, and the `{in}/{out}` token form at 60 to 79 columns; the composer edits at the caret with Left, Right, Home, End, Delete, word, kill, yank, undo, and history keys, wraps long drafts, and places the terminal caret; a cancelled tool call no longer leaves a `working` row; and a quit with an unsent draft asks `[y] Discard the draft and quit` or `[n] Keep editing`, with focus on `Keep editing`. The session now tells clients when a background job starts and settles.
- Keep text from `before_turn` hooks in a separate reminder entry after the user message. The model reads it as user text, compaction counts it, and the terminal draws no row for it. Named rule reminders also enter model context.
- Add `elapsed_ms` to tool results: a tool result entry and the `tool_settled` update now carry the milliseconds the tool ran, measured on a monotonic clock and without the time spent waiting for an approval answer. It is absent for a call that never ran and for sessions written before this change.
- Fix starting a child session that cannot open: the agents service now returns the cause, such as a child name already used in the workspace, instead of a bare cancelled reply. Two children started at the same time with the same name can no longer both claim it.
- Let an approved extension call run for its granted commands until its own run job ends. Other calls, other commands, and other directories still ask. A child session now closes after a failed wait.
- Fix task artifacts written through the sidecar service: they now land in a directory named by the job id as it is displayed. Before, the directory used the debug form `JobId(...)`, so a reader given the job id could not find them.
- Add `note` to `CoveredEntry` so compactors can retain a reminder's journal position and text. Before-turn and named rule reminders also contribute model context through `covered_context()`.
- Add `ExtensionBuilder::fallback_compactor`: a fallback compactor runs after every primary compactor, whatever the extension's origin, so a bundled battery's compactor gets its turn before dal's text summary. The built-in `summary` compactor is now a fallback; the chain is `remote`, then the batteries, then `summary`. `Extension::is_fallback_compactor` reports the registration.
- Fix the question dialog for selections: a long preview no longer hides the choices, each choice shows its description, and Page Down and Page Up scroll the preview.
- Fix reading extension manuals with the `read` tool: an extension that registers doc pages now serves them at its own `<name>://` address through `ManualScheme`, which `dalgon` re-exports.
- Fix the `run` service: it now keeps the stdout prefix a caller asks for. Before, every run reported an overflow with an empty prefix, so extensions that read command output, such as the review battery, could not read it.
- Fix the terminal help key and `/copy`: F1 now opens the key list instead of failing with "no command registered for keys", every key in the list shows its name, and `/copy` puts the last reply on the clipboard and reports `copied N characters` instead of printing debug text.
- Fix text typed in the terminal while a turn runs: it now steers the turn at its next safe point and shows `1 message queued for the next reply`. Before, the terminal refused it with a turn mismatch error and dropped the text.
- Fix the terminal status line and the first-run banner: a model named in `dal.toml` now shows in the status line and clears the sign-in banner. The status line also shows token use and cost, writes the workspace path with `~`, and shows `waiting for you` in full.
- Fix settled tool cards in the terminal: each call shows one `ok  {name} {summary} · {duration}` card, or `failed  ...`, with no leftover `working` row and no `\t` in the summary.
- Fix replies that stream in several parts: the journal now keeps adjacent text as one block, so the terminal shows one paragraph instead of one row per part.
- Fix multiline drafts in the terminal: a pasted draft shows each line on its own row in the composer instead of one row with `\n` inside it.
- Fix the saved-session line on exit: an unnamed session now shows its id in `run dalgon -r <id> to resume`. Before, the line named "session", which resumed nothing.
- Fix the resume hint for names with shell characters. `/name` and the saved-session exit line print a name bare when it holds only ASCII letters, digits, `.`, `_`, and `-`, and in double quotes when it also holds spaces or apostrophes. Any other name, and any name that starts with `-`, prints the session id, which `-r` resumes. Before, `$(...)` and backticks in a name ran, and `%VAR%` expanded, when the hint was pasted; the exit line also printed names unquoted, so a name with a space or an apostrophe did not paste as one argument.
- Fix grant denials for extension services: the error now names the plugin and the service and says to approve the grant request, then try again. Before, it said only that the extension's services are not granted.
- Fix the services `run` operation under an `ask` approval policy: it now opens an approval request like the exec tool does, runs after approval, and fails closed with a clear message when no front end can answer. Before, it skipped the approval question and ran, or failed with a bare denial.
- Fix the WebSocket server for oversized messages: a message over 16 MiB now ends the connection without a close frame, as the protocol documents. Before, the server sent a normal close frame.
- Fix the agents `cancel` operation: a failure to close the child session is now returned to the caller instead of being reported as a successful cancel.
- Fix turns that failed with "unknown model" when the configured model was written with a family prefix, such as `openai-responses/gpt-6`, and the provider's model list was not available. The turn now finds the model by its family and runs.
- Fix question titles and clipped file links in the terminal: a question title always shows as text, even with diagram rendering on, and a file link in a clipped tool result keeps its full destination.
- Fix tool and command panics that happen before the work starts. The call now returns an error and the session stays usable.

- Add `cancel_queued`, a command that removes one queued follow-up by its turn id. The `queued` reply to a follow-up now carries that `turn`; a queued steer still has none. The removed text comes back in a `discarded` notice, and an id that is not queued is rejected and changes nothing. A follow-up that has already started cannot be removed; cancel its turn instead.
- Fix provider requests built from a journal that holds more than one result for the same tool call. The request now carries only the first result for each call of a model response, so providers no longer reject it. The journal is not changed.
- Fix child sessions: a child started with a `tools` list now sees and can call only those tools, on every turn and after every reload, through the model, scripts, and extension services. An empty list gives the child no tools, and a start with no list leaves it unrestricted. A child also starts under the approval mode its parent runs under now, not the configured default.
- Add the `-32009` error code ("server draining") to the dal protocol. A WebSocket connection answers every new request with it once the listener begins to stop, lets running requests finish for up to one second, and then closes. A request to a host that has shut down now answers `-32009` instead of `-32603`.
- Fix the text the model reads when a `tool_call` hook blocks a call: it now starts with `blocked by <extension>:` so the model and the user can see which extension refused the call and why.
- Add `image_bytes_elsewhere` to compactor input: the bytes of the images that stay in the request, so a compactor can keep its images within a request size budget.
- Fix an HTTP 413 "request too large" answer from a provider: dal now compacts the history and retries the turn, as for a context-overflow error, instead of failing the turn.
- Price model requests by context size: a model can list request-wide price tiers in its prices (`dal.toml` and the bundled price table), and dal uses the highest tier that the request's input tokens exceed.
- Add opt-in fenced-diagram rendering for terminal transcript rows and ask previews: the `/settings` picker toggles `tui.diagrams` for the session and a save row persists it to `dal.toml` through the standard product configuration; CLI-backed diagram renders run on background workers with a pending placeholder, and pixel results draw through the terminal's image protocol (kitty, sixel, or iTerm2) instead of a text card.
- Fix ACP permission requests: `toolCall.toolCallId` now carries the provider's tool-call id that the tool-call updates announced, so a client can match a permission request to its call. Approval questions gain an optional `call` member for this; encoded questions without it still decode, and a question with no call keeps the request id.

- Persist image-bearing compaction parts and their extension letters atomically, replay them from the journal, and resolve letter history sources after resume; pass the selected catalog image profile, retained-image count, and carried summary to compactors.
- Accept mapped MCP tool names in registration, dispatch, approval, and replay; send provider-safe aliases to providers and restore internal names on streamed calls.
- Fix extension questions raised while no front end that can answer is attached, such as print mode, `--json`, child sessions, the router, and A2A: the question now takes its no-answer default at once instead of waiting for its time limit. A question raised while a front end is attached keeps waiting, up to its time limit, if that front end disconnects, so a client can reconnect and answer it. The time limit counts from when the question is raised and nothing extends it. Router and A2A runs no longer wait for approvals: they deny them at once, as print mode does.
- Fix RPC and remote clients that never said they answer: a client now answers approvals only when its `initialize` list names `approval`, and extension questions only when it names `ask` or `question`. Anything else fails closed at once, an approval is denied and a question takes its default, while declared kinds still wait for the client's answer.
- Fix the approval timeout text: it now reads `Permission denied: <tool> needed approval and no one answered within <n> s.`, with the colon the manual names. An approval that nobody could answer is now reported as unavailable, not as a refusal by a person; it is still denied.
- Fix `tool_call` hook rewrites: the rewritten arguments now pass the same checks as the model's original arguments before the tool runs. A rewrite the tool rejects, such as a path outside the workspace, ends the call with the same "invalid arguments" error instead of reaching the tool.
- Fix a panic in a tool or slash command handler: it no longer ends the session's driver task and leaves the turn or command hanging. The tool call settles with an error that names the tool and carries the panic message, and a command answers an error that names the command. The session keeps running, and ACP clients see the failure instead of waiting forever.
- Fix `dalgon acp`, `dalgon rpc`, and `dalgon app-server` over standard input and output: the command now moves the protocol onto private copies of standard input and output and points the shared streams at the null device and standard error. Output from tool children, extensions, or libraries can no longer corrupt the JSON-RPC stream on Unix; it appears on standard error. Windows keeps the shared streams.
- Add `DAL_THREAD_ID` and `DAL_TOOL_CALL_ID` to the environment of `exec` children. They carry the session id and the tool call id, and they replace values with the same names in the captured environment. A value that contains a NUL byte is left out.
- Fix `patch` text matching on large files: the search now does linear work instead of rescanning the file for each match. When `old` has no exact match, the patch also accepts the same text with CRLF line ends, copied line-number prefixes, curly quotes, dashes, special spaces, or trailing spaces. Indentation must still match.
- Fix Anthropic replay after the system prompt or tool list changes: signed thinking blocks from the old prefix are dropped instead of failing the request with "Invalid signature in thinking block".
- Fix Anthropic replay from older sessions: signed thinking blocks without a recorded prefix are dropped unless they are needed to continue an active tool call.
- Fix Claude login when the preferred callback port is busy: login now uses a free loopback port and sends that port in the redirect address, instead of falling back to pasting the code.
- Fix terminal presentation: the queued and steering counts now come from the host's turn state, multiline question titles keep each line, wrapped transcript rows keep their links, and file paths in settled rows are clickable OSC 8 links.
- Fix terminal completion cycling. Tab moves through the matches starting from the first match, Shift+Tab moves back starting from the last, and a single match accepts at once.
- Fix stale completions after the draft changes. Recalling an earlier prompt and a successful return from the external editor drop the old matches so the next Tab completes the text on screen. Cancelling the editor keeps the draft as it was.
- Fix selection lists at small heights. A selection list keeps its title, the focused choice, and its actions on screen instead of hiding the focused row or spilling past the space it has.
- Fix the escape key after idle. A lone Esc waits a brief window so Alt key chords that arrive in pieces still decode instead of splitting.
- Fix terminal cancellation reporting. A refused cancellation request reports
  its own error while that turn is still running, and a transport error while
  waiting reports that error, instead of finishing as if nothing failed. When
  the authoritative view shows the turn already finished, quit and resume
  complete normally with no cancellation line. A lost replay while waiting, a
  closed subscription, or no confirmation before the wait runs out reports that
  the cancellation could not be confirmed and asks to resume the session to
  check its state. Resuming after such a failure closes both the old session
  and the opened replacement. A turn whose own end reports cancelled still
  prints the inline cancellation line; any other end prints no line, and
  fullscreen never prints the inline line. On quit, an earlier terminal error
  still wins over a later failure to print the cancellation line.
- Initial release.
- Add extension status consumers. A registered status kind now publishes an `ext_status` update (`busy` or `quiet`, with optional text) only when it changes; `Agent::ext_status`, `Agent::poll_status`, `Agent::is_quiet`, and `Host::is_quiet` read it. The terminal shows busy extensions above the status line when the layout has room, ACP sends a `status` notice, A2A adds `dal.status` metadata, and the router sends nothing. `dalgon --json` writes its line only after every status kind is quiet; print mode waits 3 seconds and exits 1 with `dalgon: extension status is not quiet` if a kind stays busy.
- Breaking: change the plugin API to Plugin and Eval v1. A plugin now publishes one `plugin` value built after `load("@dal/v1", "dal")`. Tools take an `input` schema, handlers are `run(ctx, args)`, each tool, hook, and model lists its operations in `uses`, and the plugin requests host services through `inject`. This replaces `dal.tool(name = ...)`, `dal.prompt_section`, and `run(args, ctx)`. To update a plugin, follow `dal://plugins`.
- Add scripted synthetic model routes to Plugin and Eval v1. The `models` map associates a plugin-local key with `dal.model(id = ..., caps = ..., run = ..., uses = [...])`; `id` is the public `namespace/route` name. `run(ctx, request)` receives the provider-neutral request and can call `ctx.models.infer` or `ctx.models.forward`. Add `infer` to the plugin's `inject` services when models call those operations. A scripted model can forward once per run; the host rejects cycles and synthetic model chains deeper than four routes.
- Add `always_apply`, `scope`, `interrupt_mode`, `repeat_mode`, and `repeat_gap` to `dal.rule`, so a plugin rule can apply every turn or fire only in chosen content, such as `scope = ["tool:exec"]`.
- Change `eval` to take `{code, uses?, data?}`. A cell inherits its operations from `eval.uses`, and `uses = []` makes it pure.
- Remove the `notify` example plugin; its behavior cannot run under the v1 hook limits.
- Fix extension records: `append_record` now writes a journal record before it returns, `records` reads only the calling extension's rows on the current leaf path, and a tool call runs as the extension that registered it, with that extension's `inject` set. Records survive resume and follow `/tree` leaf moves.
- Move `dal-agent` diagnostics from standard error to `tracing` events on the `dalgon.agent` target: stuck driver batches, turn openings, journal append failures, idle provider streams, slow infer steps, and file-descriptor limit changes no longer write to the terminal. `DAL_DEBUG` process-edge timing stays on standard error.
- Breaking Rust API: remove the exported `BUILTINS` and `MODE` command records. Iterate `dal_ext::commands::specs()` for the ordered metadata (with `mode` last), or call `spec("mode")` for that record.
