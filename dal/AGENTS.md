# dal engineering contract

Maintain this workspace as the reusable agent platform. Apply the repository contract
first. Then use this file for dal-specific architecture and behavior.

## Ownership boundaries

- Keep `dal-core` pure. Put shared values, strict configuration, codecs, the session
  fold, and approval policy there. Do not put I/O, task spawning, registries, provider
  calls, or ambient process state in it. Every effect must remain visible to the host.
- Keep `dal-store` as the only owner of durable session state. Route journal, blob,
  sidecar, session lock, fork, clone, name, archive, and repair operations through it.
  A second persistence path can acknowledge data that recovery cannot reproduce.
- Keep `dal-provider` as the only owner of API-family wire formats, authentication,
  model resolution, prices, usage normalization, retry classification, and provider
  replay fixtures. Do not leak provider-specific events into the agent loop.
- Keep `dal-agent` as the only owner of live sessions. Route every command, answer,
  tool result, hook result, job event, and provider event through its actor and pure
  fold. Shared mutable session state creates order-dependent replay.
- Keep `dal-tools` limited to tool behavior. Use the host services for approval,
  paths, parsing, processes, cancellation, blobs, and progress. A tool must not open a
  second authority path.
- Keep first-party, non-tool loop features in `dal-ext`. Build prompts, commands,
  compaction, rules, the judge, skills, letters, guard behavior, and self-documentation
  through the same extension records that external adapters can use.
- Keep `dal-star` as an adapter over the extension API. Do not add a second registry,
  grant store, service implementation, or prompt pipeline for Starlark.
- Keep `dal-tui` as a client of `Host` and `Agent`. Render only structured `Update` and
  `View` values. A UI that reads the journal bypasses replay and remote parity.
- Keep `dal-wire` as a transport adapter for the public host contract. Reuse core
  request, update, error, and schema types. Copied wire-only models drift.
- Keep the `dalgon` crate as the composition and process edge. Resolve environment,
  paths, signals, CLI flags, product assembly, and exit codes there. Do not resolve
  them in libraries.

## State and persistence

- Apply session input in this order: validate, fold, execute ordered effects, await
  each durable receipt, then publish updates. Never expose a state transition before
  its journal record is durable.
- Keep journals append-only. Represent compaction, leaf moves, settings, repairs, and
  terminal outcomes as records. Do not rewrite or delete history to change the current
  view.
- Give one session actor exclusive ownership of one journal handle. Deduplicate opens
  in the host. Use the cross-process session lock. Two writers cannot preserve
  sequence, tree, or exactly-once guarantees.
- Preserve monotonic `gen` and `seq` replay. Return `Resync` when a client cursor
  precedes the replay ring. Never invent missing updates. Never use wall-clock order.
- Resolve each user request exactly once. Bind the answerer identity when the request
  opens. Reject later answers. Apply the declared fail-closed default when no
  controller can answer.
- Keep turns total. Every opened turn must receive one terminal record, including
  cancellation, crash repair, compaction failure, and provider failure.

## Authority and extensions

- Deny extension services by default. Treat a declared `ServiceSet` as a request, not
  an authorization token. Only a host-minted `Caller` and grant may invoke services.
- Mint callers, grants, and approved tool proofs only inside the runtime. Keep approved
  values move-only. Consume each approved value once. Code must not forge or reuse
  authority.
- Scope grants to the caller, origin, service set, session, and call where applicable.
  Revoke a call-scoped run grant when its job ends.
- Load user plugins only from the data directory and only from configured directories.
  Do not load workspace plugins. Opening a repository must not execute its code.
- Validate a complete plugin generation before publishing it. Publish one atomic
  generation. Snapshot it at turn start. Keep that snapshot for the whole turn. A
  reload failure must keep the prior generation and open calls intact.
- Keep registration names and origins explicit. Reject duplicate tools, commands,
  skills, rules, schemes, models, status kinds, and singleton services. Each error must
  name both claimants.
- An extension can register these surfaces: tool, command, hook, observer, stream
  watcher, status kind, scheme, and model. Each registered surface must reach a live
  dispatch path with a test. Treat a registered surface that nothing dispatches as a
  defect. Wire it or delete it. Never silence it with a lint expectation.
- Keep built-in Rust extensions capability-free only for the exact services declared
  safe for built-ins. Preserve grant checks for user and bundled Starlark code.
- Keep hook order deterministic. Enforce deadlines and cancellation on guarding hooks.
  Observe-only hooks must not block the turn.

## Commands, tools, and jobs

- Preserve one command path for TUI, print, JSON, ACP, RPC, router, and A2A clients.
  Front ends must not implement command behavior.
- Classify a tool call from its validated arguments before admission. Batch read calls.
  Run every `patch` and `exec` call one at a time. Preserve call order.
- Ask before every `patch` and `exec` by default. Apply the `ask`, `edits`, and `all`
  approval ladder in the fold. Deny when a headless client cannot answer.
- Keep `patch` transactional. Parse one selected dialect into the shared edit IR.
  Validate anchors against bytes the session has seen. Stage every file. Run
  observers. Ask once with the complete preview. Then commit atomically. A failure
  must leave all target files unchanged.
- Keep edit-style selection on the provider request. Record the selected dialect with
  the call. Render historical calls in their recorded dialect. Model switches must not
  reinterpret history.
- Start every process through the job door. Close stdin. Set the fixed noninteractive
  environment. Capture bounded output durably. Own the process group. Apply the
  timeout and cancellation kill ladder.
- Keep job and child-session queues bounded. Preserve backpressure, FIFO admission,
  structured ownership, cascade cancellation, and one terminal event per job.
- Keep tool progress replaceable by call ID. Keep final output durable. Large image or
  process output must become a blob or job log. It must not expand replay memory.

## Providers, configuration, and prompts

- Support OpenAI Chat Completions, OpenAI Responses, OpenAI Codex, and Anthropic through
  one normalized provider contract.
- Keep provider requests byte-stable under replay. Test stream cuts, unknown events,
  retries, usage, native compaction, tool calls, and authentication against recorded
  fixtures before changing a wire encoder.
- Decode local configuration, plugin input, protocol requests, and stored format
  versions strictly. Reject unknown local keys. Tolerate unknown provider and MCP reply
  fields and unknown protocol update variants. External producers evolve separately.
- Assemble the system prompt deterministically. Keep the stable prefix independent of
  environment data. Read `AGENTS.md` files in order from root to cwd. Read `SYSTEM.md`
  only from the data directory. Preserve the documented size and truncation limits.
- Keep image letters reversible. Store the exact source bytes. Expose them through
  `letter://`. Use complete text instead when rendering or budget checks fail.
- Run remote compaction before local summary compaction. Cut only at completed
  user-turn boundaries. Keep immutable prompt sections outside the compacted span.
  Append one compaction record. Preserve all earlier journal entries.

## Clients and terminal behavior

- Preserve one `Host` and `Agent` operation set in process and over RPC. Add a
  capability to that contract before adding a front-end-only escape hatch.
- Decode requests strictly within the negotiated protocol version. Let update receivers
  ignore unknown fields and variants. Then newer servers can work with older clients.
- Keep ACP, RPC, HTTP router, WebSocket, A2A, and the Codex adapter as mappings over the
  same operations, request broker, replay, blobs, grants, and cancellation semantics.
- Preserve the letterpress terminal model. Write committed transcript rows once. Repaint
  only the live block. Never clear the full display. Never use the stock inline
  viewport path that duplicates scrollback.
- Keep rendering pure over `View`, terminal dimensions, and confirmed capabilities.
  Preserve synchronized frame brackets, per-region dirtiness, coalescing, crash
  restore, and the performance limits in `../CONCEPTS.md`.
- Preserve meaning without color, motion, mouse input, image support, or modern key
  reporting. Use the role tokens and contrast gates in `../DESIGN.md`. Do not encode a
  state in decoration alone.

## Verification and release

- Reach every public error variant in tests. Test cancellation, partial failure,
  restart, stale answers, torn writes, unknown external fields, and limit boundaries.
- Preserve the scale gate. More than 500 child sessions and more than 200 concurrent
  process jobs must remain responsive under the documented memory, file-descriptor, and
  keypress-latency budgets.
- Run provider replay tests for provider or loop changes. Run the ignored live suite
  only with the named credentials. Missing credentials must fail with a useful name.
- Run TUI snapshots and PTY tests for terminal changes. Run protocol conformance tests
  for any `Host`, `Agent`, schema, transport, or wire change.
