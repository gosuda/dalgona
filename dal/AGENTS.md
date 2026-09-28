# dal engineering contract

Maintain this workspace as the reusable agent platform. Apply the repository contract
first, then use this file for dal-specific architecture and behavior.

## Ownership boundaries

- Keep `dal-core` pure. Put shared values, strict configuration, codecs, the session
  fold, and approval policy there. Do not put I/O, task spawning, registries, provider
  calls, or ambient process state in it; every effect must remain visible to the host.
- Keep `dal-store` as the only owner of durable session state. Route journal, blob,
  sidecar, session lock, fork, clone, name, archive, and repair operations through it;
  a second persistence path can acknowledge data that recovery cannot reproduce.
- Keep `dal-provider` as the only owner of API-family wire formats, authentication,
  model resolution, prices, usage normalization, retry classification, and provider
  replay fixtures. Do not leak provider-specific events into the agent loop.
- Keep `dal-agent` as the only owner of live sessions. Route every command, answer,
  tool result, hook result, job event, and provider event through its actor and pure
  fold; shared mutable session state creates order-dependent replay.
- Keep `dal-tools` limited to tool behavior. Use the host services for approval,
  paths, parsing, processes, cancellation, blobs, and progress; a tool must not open a
  second authority path.
- Keep first-party, non-tool loop features in `dal-ext`. Build prompts, commands,
  compaction, rules, the judge, skills, letters, guard behavior, and self-documentation
  through the same extension records available to external adapters.
- Keep `dal-star` as an adapter over the extension API. Do not add a second registry,
  grant store, service implementation, or prompt pipeline for Starlark.
- Keep `dal-tui` as a client of `Host` and `Agent`. Render only structured `Update` and
  `View` values; reading the journal from the UI bypasses replay and remote parity.
- Keep `dal-wire` as a transport adapter for the public host contract. Reuse core
  request, update, error, and schema types; copied wire-only models drift.
- Keep the `dalgon` crate as the composition and process edge. Resolve environment,
  paths, signals, CLI flags, product assembly, and exit codes there rather than in
  libraries.

## State and persistence

- Apply session input in this order: validate, fold, execute ordered effects, wait for
  each durable receipt, then publish updates. Never expose a state transition before
  its journal record is durable.
- Keep journals append-only. Represent compaction, leaf moves, settings, repairs, and
  terminal outcomes as records; do not rewrite or delete history to change the current
  view.
- Give one session actor exclusive ownership of one journal handle. Deduplicate opens
  in the host and use the cross-process session lock; two writers cannot preserve
  sequence, tree, or exactly-once guarantees.
- Preserve monotonic `gen` and `seq` replay. Return `Resync` when a client falls behind
  the replay ring; never invent missing updates or use wall-clock order.
- Resolve each user request exactly once. Bind the answerer identity when the request
  opens, reject later answers, and apply the declared fail-closed default when no
  controller can answer.
- Keep turns total. Every opened turn must end with one terminal record, including
  cancellation, crash repair, compaction failure, and provider failure.
- Use RFC 3339 UTC timestamps for stored events and monotonic clocks for durations;
  wall-clock changes must not alter timeouts or ordering.

## Authority and extensions

- Deny extension services by default. Treat a declared `ServiceSet` as a request, not
  an authorization token; only a host-minted `Caller` and grant may invoke services.
- Mint callers, grants, and approved tool proofs only inside the runtime. Keep approved
  values move-only and consume each once; code must not forge or reuse authority.
- Scope grants to the caller, origin, service set, session, and call where applicable.
  Revoke a call-scoped run grant when its job ends.
- Load user plugins only from the data directory and only from configured directories.
  Do not load workspace plugins; opening a repository must not execute its code.
- Validate a complete plugin generation before publishing it. Publish one atomic
  generation, snapshot it at turn start, and keep that snapshot for the whole turn;
  reload failure must leave the prior generation and open calls intact.
- Keep registration names and origins explicit. Reject duplicate tools, commands,
  skills, rules, schemes, models, status kinds, and singleton services with errors that
  name both claimants.
- Route file, process, network, environment, inference, child-session, job, turn,
  sidecar, ask, and MCP effects through typed services. Do not pass raw host handles to
  an extension.
- Keep built-in Rust extensions capability-free only for the exact services declared
  safe for built-ins. Preserve grant checks for user and bundled Starlark code.
- Keep hook order deterministic. Enforce deadlines and cancellation on guarding hooks;
  observe-only hooks must not block the turn.

## Commands, tools, and jobs

- Preserve one command path for TUI, print, JSON, ACP, RPC, router, and A2A clients;
  front ends must not implement command behavior.
- Classify a tool call from its validated arguments before admission. Batch read calls.
  Run every `patch` and `exec` call one at a time, and preserve call order.
- Ask before every `patch` and `exec` by default. Apply the `ask`, `edits`, and `all`
  approval ladder in the fold, and deny when a headless client cannot answer.
- Keep `patch` transactional. Parse one selected dialect into the shared edit IR,
  validate anchors against bytes the session has seen, stage every file, run observers,
  ask once with the complete preview, then commit atomically. A failure must leave all
  target files unchanged.
- Keep edit-style selection on the provider request and record the selected dialect
  with the call. Render historical calls in their recorded dialect; model switches must
  not reinterpret history.
- Start every process through the job door. Close stdin, set the fixed noninteractive
  environment, capture bounded output durably, own the process group, and apply the
  timeout and cancellation kill ladder.
- Keep job and child-session queues bounded. Preserve backpressure, FIFO admission,
  structured ownership, cascade cancellation, and one terminal event per job.
- Keep tool progress replaceable by call ID and keep final output durable. Large image
  or process output must become a blob or job log instead of expanding replay memory.

## Providers, configuration, and prompts

- Support OpenAI Chat Completions, OpenAI Responses, OpenAI Codex, and Anthropic through
  one normalized provider contract. Reuse the selected HTTP and WebSocket stacks; do
  not add a parallel client for one feature.
- Keep provider requests byte-stable under replay. Test stream cuts, unknown events,
  retries, usage, native compaction, tool calls, and authentication against recorded
  fixtures before changing a wire encoder.
- Decode local configuration, plugin input, protocol requests, and stored format
  versions strictly. Reject unknown local keys. Tolerate unknown provider and MCP reply
  fields and unknown protocol update variants; external producers evolve separately.
- Capture environment and workspace paths once at the process edge. Pass typed snapshots
  inward; libraries must not read process cwd, environment variables, stdin, signals,
  or spawn commands directly.
- Assemble the system prompt deterministically. Keep the stable prefix independent of
  environment data, walk `AGENTS.md` from root to cwd in order, read `SYSTEM.md` only
  from the data directory, and preserve the documented size and truncation limits.
- Keep image letters reversible. Store the exact source bytes and expose them through
  `letter://`; fall back to complete text when rendering or budget checks fail.
- Run remote compaction before local summary compaction. Cut only at completed user-turn
  boundaries, keep immutable prompt sections outside the compacted span, append one
  compaction record, and preserve all earlier journal entries.

## Clients and terminal behavior

- Preserve one `Host` and `Agent` operation set in process and over RPC. Add a capability
  to that contract before adding a front-end-only escape hatch.
- Decode requests strictly within the negotiated protocol version. Let update receivers
  ignore unknown fields and variants so newer servers can talk to older clients.
- Keep ACP, RPC, HTTP router, WebSocket, A2A, and the Codex adapter as mappings over the
  same operations, request broker, replay, blobs, grants, and cancellation semantics.
- Preserve the letterpress terminal model. Write committed transcript rows once and
  repaint only the live block. Never clear the full display or use the stock inline
  viewport path that duplicates scrollback.
- Keep rendering pure over `View`, terminal dimensions, and confirmed capabilities.
  Preserve synchronized frame brackets, per-region dirtiness, coalescing, crash restore,
  and the performance limits in `../CONCEPTS.md`.
- Preserve meaning without color, motion, mouse input, image support, or modern key
  reporting. Use the role tokens and contrast gates in `../DESIGN.md`; do not encode a
  state in decoration alone.

## Verification and release

- Add a regression test at the contract owner. Use deterministic scripted providers,
  replay servers, in-memory transports, temporary stores, PTYs, and property tests;
  do not replace the real boundary with mocks.
- Reach every public error variant in tests. Test cancellation, partial failure,
  restart, stale answers, torn writes, unknown external fields, and limit boundaries.
- Preserve the scale gate: more than 500 child sessions and more than 200 concurrent
  process jobs must remain responsive under the documented memory, file-descriptor, and
  keypress-latency budgets.
- Run provider replay tests for provider or loop changes. Run the ignored live suite
  only with the named credentials; missing credentials must fail with a useful name.
- Run TUI snapshots and PTY tests for terminal changes. Run protocol conformance tests
  for any `Host`, `Agent`, schema, transport, or wire change.
