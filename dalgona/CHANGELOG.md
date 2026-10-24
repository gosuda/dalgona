# Changelog

## Unreleased
- Show dalgona's name in terminal headers, prompts, and session notices.
- Edit documented, non-secret product settings through `/settings`. Valid
  edits save immediately to `dal.toml` with owner-only permissions and preserve
  comments and unrelated settings. Rows show saved and running values when
  they differ. Restart dalgona to apply product changes. Remote clients keep
  these host-owned settings out of their local picker.
- Run `dalgona dev run` scenarios with dalgona's defaults, configuration,
  and batteries instead of the base dal product.
- Resolve eval configuration and data directories through the same process-edge
  rules used by interactive sessions, including the Windows home-directory fallback.
- Read work progress through the work battery's existing `todo://terminal`
  document. Scheme resolvers now use their registered owner's identity and
  permissions, not the requesting extension's identity.
- Show review activity while a review is running and clear it when it ends.
- Deliver pending quality offers to the model once, with bounded notices
  and file reads. Keep unreadable offers pending without blocking later
  offers, and keep concurrent sessions' quality state separate.
- Keep held quality lanes from advancing repetition history or reporting findings.
  Apply each session's recorded lane settings before extension defaults.
- Report an MCP process crash after a partial write instead of masking the error
  as cancellation.
- Canceled queued prompts no longer delay goal continuation. The orchestration
  battery drops canceled queued input without treating it as a user turn.
- Persist worktree status records (`status.txt`) atomically through the sidecar
  artifact service during open-time salvage before removing a salvaged worktree.
  A failure to persist either the patch or status record preserves the worktree
  and its registration in place and names the path in the salvage summary notice.
- Compile and run the orchestration constants truth-table tests. The test
  module was never declared, so none of its tests ever ran. The aliases check
  is now a source scan that fails on any twin literal of an owner constant,
  and a new test holds the monitor clamps the page states to the ranges the
  config enforces. Other new attack tests pin exact edges for the web,
  history record, review, eval pack, run repeats, collapse, control-leak,
  goal objective, and rule-set limits.
- Run the released-artifact gates (`dalgona-release-10`, `-11`, `-12`) only
  when `DALGONA_RELEASED_ARTIFACTS` is set. They install the published crate
  and the tag's release assets, so a plain `cargo test` before a release no
  longer fails on artifacts that cannot exist yet. The tag-only CI job sets
  the variable. Each gate now reads the expected version from the workspace
  metadata instead of a fixed `0.1.0`.
- Stop waiting on an MCP tool call when its turn is cancelled. The client
  tells the server the request is abandoned and the cancelled turn ends at
  once instead of holding until the call deadline.
- Cancel and join the blocking render worker when history compaction times
  out before reporting the timeout.
- Cooperatively cancel dropped history runs at checkpoints so workers stop
  further work, without hard preemption or teardown joins.
- Reject `web_search` `count` arguments outside 1 to 20 before verifying
  credentials or initiating network requests.
- Preserve isolated worktrees when a workflow exceeds its shutdown grace period.
  Save each recovery path before completing session close instead of aborting
  the recovery writer with a competing timeout.
- Coordinate parent and child responses in the live-run review gate instead
  of sharing a response queue across sessions.
- Update the shared dependency lockfile and use proptest 1.12.0 for battery
  and product gate tests.
- Breaking Rust API: replace `Battery::reset_assistant(session)` with
  `Battery::begin_assistant_turn(session, turn)`. Pass the same turn ID
  across tool rounds so the claim check retains the whole assistant turn.
- Breaking Rust API: `orchestration` now returns `OrchestrationParts`.
  Register its `extension` and pass its `view` to `review_with_view`.
  Review then reads the live work state owned by that orchestration instance.
- Avoid compiling unrelated stream conditions when checking user overrides
  for detector lanes. Each session still reads the current user rules.
- Finish the judged battery: auto-thinking now yields to a level you set with `/thinking` or `SetThinking`, including a level already in a resumed session's journal; an anchored ask is never classified in a child session and is allowed unjudged when its arguments are not valid JSON; the claim check reads the whole turn's assistant text across every tool round instead of the last request alone; search ranking asks one score question per repeated path and skips candidates with a blank display line; prompt dedup consults the outstanding reminder closest to the candidate (the newest on a tie) instead of the newest one; `JudgedParts` exposes its `battery` so other batteries can call the shared admission function; and the `dalgona://judged` page now lists every judge prompt, the shared context envelope, the verdict mapping, the ledger feature values, the breaker notice, the ask tool's `explicit` field, and each case where a feature stays silent.
- Complete the ask battery: `[ask] timeout` in `dal.toml` (default `30m`, clamped into 1m to 120m at load with one startup notice when clamped) bounds every raised question; concurrent asks queue instead of failing with "another question is already open"; the planner dispatches `ask` alone even beside read-only calls; answers travel as `{"selected":[...]}` and `{"text":...}` payload objects with a malformed answer reported as one named notice; an empty submission is a dismiss; the TUI dialog gains the always-present Other free-text row, tab movement, and a minutes-left countdown; the schema publishes its full bounds and the judged explicit flag. A losing answer now reports exactly `request <id> was already resolved by <client>.`.
- Add Tavily as a second `web_search` provider and refuse private addresses in `web_fetch`. Set `[plugin.web] provider = "tavily"` (a Tavily config that keeps the default `api_key_env` reads `TAVILY_API_KEY`) and, optionally, `search_depth = "advanced"`; results gain `published_date` when Tavily sends one, and unknown reply fields are dropped. The key never appears in a result or error: every occurrence is replaced with `***`. `web_fetch` now refuses loopback, link-local, RFC 1918, unique-local, unspecified, and other non-public targets, on every hop including redirects, with `web_fetch: <host> is a private address; fetch public URLs only.` This is a behavior change: fetching `localhost` or an internal URL no longer works; use `exec` with `curl`. The check classifies the resolved address, not the name; an `http` hop connects to the address that passed, so a name cannot rebind between the check and the connection. A transport failure now reports `web_fetch failed: no response from <url>` instead of `HTTP 0`. The `web_search` tool description names the configured provider.
- Add `dalgona eval <pack>`: run a Starlark eval pack (`<name>.eval.star`) of cases, each one prompt in a fresh ephemeral session over a seeded temporary workspace, scored by a pure check or, when the judge gate is on, by a judge question. A pack sees exactly the four `evalpack` builtins (`pack`, `case`, `judge`, and the check's `workspace.read`) and no `dal.*` or `ctx`. No pack ships with the product: a name resolves to `<data root>/evals/<name>.eval.star` and a path loads directly. Each case has a 300 s wall clock; a case that exceeds it fails as `case <id> timed out after 300 s` and the run continues. A judge case under a gate that is `off`, or `auto` without credentials, is `skipped` and never counts as failed. `--repeats`, `--case`, and `--model` select the run; `--against <config.toml>` runs every case a second time with that file layered over your config and prints paired deltas. Every run appends one line to `runs.jsonl` and the report is written to `report.json` under `<data root>/evals/runs/<pack>/<run id>/`. Exit 0 when a report was written, 1 for a pack load or usage error, 130 on SIGINT with the partial report and the remaining cases `skipped`. Read `dalgona://eval`.
- Give the review battery an explicit provider contract: each round probes the reviewer route's credentials, then sends one request under a 120 second deadline. An empty `reviewer_model` now uses the session's model route; before, it sent an empty model id. A route without credentials, a provider error, and a missed deadline each fail the round as `reviewer call failed: <cause>` with no record, and cancelling the call appends nothing. A reply that reports token usage posts one `review` notice with the counts. A stored finding `detail`, an error cause, and a tool `focus` over 500 bytes now end with a truncation marker instead of a silent cut.
- Declare the `[plugin.orchestration.isolation]` sub-battery key (default `enabled = true`) so the worktree-isolation surface is configured like every other sub-battery instead of failing as an unknown key; register the `dalgona://agents`, `dalgona://workflows`, and `dalgona://goal` manual pages alongside a constants truth table on `dalgona://orchestration` that is generated from the enforced constants, with a gate that reads each value from the code and fails when the page drifts; expose one owned read-only view of each session that reports the live-work counts, the durable goal, and the status payload (whose objective preview is cut to 32 characters and whose snapshots emit only on change at most twice per second), clearing every entry when the session closes, with no process-wide registry; wake waiting job reports after 2 s quiet, after 16 wait, or after the oldest waits 60 s; refuse a review round while orchestration work is still live in the session, instead of reviewing a diff that is still moving; and cover it with product-boundary gates for shutdown bounds, exactly-once run report delivery, goal sidecar durability, exact goal command refusal texts, the review refusal, the isolation key and coherence refusals, and opt-in stress gates (`DALGONA_ORCH_STRESS=1`) holding the 500-item pool and the exec-plus-monitor fan-out inside the process contract.
- Use the session's configured judge and model route for judged features and
  history consolidation. Anchored asks now receive their verdict instead of
  silently skipping the judge, and history can consolidate a resumed session.
- Require MCP stdio declarations to use `launch.kind` with `command`, `npx`,
  or `uvx`; HTTPS declarations retain `url`. This is a breaking declaration
  change. Grant and tool previews show the validated public target, not
  command operands, environment names or values, URL paths or queries, or
  remote arguments. Changed declarations require their own grant.
- Check the shared judge without consuming a model response at startup.
  Judged rules now enforce verdicts through dal's bounded interrupt and
  reminder path. A rejected match reports a pass note, not a rule fire.
- Rename the man-page example to `render-dalgona-man`. Use `cargo run -p dalgona --example render-dalgona-man -- <out-dir>`; dal keeps `render-man`, so both generators have distinct build outputs.
- Correct the manual's installer URLs to use dalgona's version-tagged asset names.
- Configure cargo-binstall to use version-tagged dalgona archives and their platform-specific binary paths.
- Include the existing `dev` command tree in the generated dalgona man pages.
- Harden MCP HTTP OAuth: require both an affirmative confirmation and a validated callback before exchanging credentials, and enforce resolved-address boundaries for discovery and token requests without following redirects.
- Fix a superseded MCP OAuth refresh storing its stale token: a refresh that completes after an interactive sign-in now yields and persists the published record instead of overwriting it.

- Bound history reuse reads: compaction now indexes prior metadata, fetches only the selected reusable PNG blobs after budget selection, and excludes skill and dream records from image counts.
- Fix orchestration job reports after an accepted wake: a failed or partial delivery acknowledgement no longer skips the session's own cleanup or drops the error. The runtime reports a `delivery` notice, keeps the unconfirmed report ids, and retries only the acknowledgement, so the wake is never repeated and no report is lost. Other delivery-poll failures are reported once per cause instead of being dropped.
- Fix `/goal` on a fresh install: the orchestration battery no longer keeps the denial from its session-start goal load, so the first goal command asks for the battery's grant and then works, and a declined grant stops the battery's idle poll from asking again until the next goal command, prompt, or tool call.
- Continue an active goal on its own: the continuation now fires right after an automatic turn, ten seconds after a user turn, at idle wakes for other sources, and after a provider error once the user speaks again, while `/goal clear` on a damaged goal file repairs it in place. A goal turn is now counted when its continuation is delivered, not when it is scheduled, so a prompt that cancels a waiting continuation leaves the counters unchanged, and a turn that ends on a context overflow blocks the goal with that reason.
- Show the exact reason when an orchestration child cannot start, such as the child depth limit, instead of a bare failure.
- Give each orchestration child one final grace prompt when it ends without a report, then fail it clearly if it stays silent.
- Keep goals across session reopen and report failed goal saves.
- Show orchestration status as a short human-readable line in the activity row instead of a raw JSON object.
- Show the plan battery status as one short line in the activity row, such as `planning · 3/5 done · writing tests`, instead of a raw JSON object.
- Draw compacted history as images: for a model that reads images, compaction now emits the older journal as labeled PNG images with exact-text `letter://` records, reuses stored letters across compactions, and keeps stored text as text. The text summary still runs whenever the image path declines.
- Run the history compactor before the text summary: the compaction chain is now `remote`, `history`, `summary`, so an image-reading model gets images instead of a summary whenever the image caps allow it.
- Fix `read dalgona://<page>`: the model can now read the Dalgona manual, not only the `dalgona docs` command.
- Include an MCP stdio server's exit status and bounded stderr excerpt when it crashes during a call.
- Fix the review battery: `/review` can now read the git status and diff of a session, so it reports findings instead of failing with a workspace-size error.
- Fix the history compaction notice for a model that reads images when this host cannot store them: it now says `history: this host cannot commit image parts.` instead of `history: the journal source is unavailable.`, and the text summary still runs.
- Run orchestration workflows in the background with bounded pools, dependency ordering, isolated patches, one final report, cancellable waits, and monitored job output.
- Fix goal saving errors: a failed write of the goal file now reports `goal: saving the goal failed: <message>` instead of a raw file error.
- Make `/abort` continue cancelling child sessions when status checks fail, report unexpected replies, and stop after a bounded sweep with active session ids.
- Fix finished orchestration children that kept their runtime until the session ended: a run now closes each child as soon as its report arrives, with or without a connected client, and shows a notice if the close fails.
- Require the user to run `/review` before a capped review can restart, and clear unused restart grants when a session closes.
- Keep compaction images within the request size limit: history compaction now subtracts the bytes of the images that stay in the request from its image byte budget, so it draws fewer images, or declines, instead of building a request the provider rejects.
- Add opt-in fenced-diagram rendering to TUI transcript rows and ask previews; Dalgona asks models for supported diagram fences when enabled.
- Assemble Dalgona's batteries as bundled Rust extensions through dal's public extension API.
- Stop the review tool at the round cap and list the findings still open; running `/review` after the cap starts a new session through the new `restart` argument, which never discards rounds in progress.
- Fix MCP servers that use OAuth when several calls get a 401 at once: one token refresh serves all of them. A refresh that fails for a temporary reason, such as a network error or a server error, no longer forces a new login, and a token from a new login replaces the old token for every connection.
- Fix MCP step-up authorization prompts repeated by concurrent tool calls: the server's "insufficient scope" reply now prompts the user once for the extra permission, and a declined or cancelled prompt answers later calls without asking again until a fresh token is stored.
- Fix `/abort` and session end with orchestration children: every queued or running child and its descendants are cancelled, and a failed cancel or list no longer stops the sweep. `/abort` reports each failure with the child id, and `agents cancel` tries every named id and reports each refusal.
- Fix `/abort` errors for missing orchestration grants: the message now names the plugin and the service and says to approve the grant request, then try again.
- Bound durable review records to 262144 bytes on both paths: a stored record that is oversized, undecodable, has round 0, or holds more than 50 findings fails the round before the reviewer runs, and a new record past the bound fails before anything is appended; refused records are never dropped or rewritten.
- Keep read-only planning enabled when saving a requested revision fails. Report the journal error without allowing edits.

## 0.1.0 (2026-09-26)

- Initial release of the headless loop, terminal, compiled batteries, wire surfaces, and manual.
