# dalgona engineering contract

Maintain this workspace as the opinionated product built on dal. Apply the repository
contract first, then use this file for Dalgona-specific composition and batteries.

## Composition

- Keep the `dalgona` crate as the composition root. Let `product()` register defaults,
  compiled batteries, bundled Starlark sources, docs, and binary aliases; do not put
  battery behavior in the entry crate.
- Register the eight compiled batteries `orchestration`, `history`, `quality`, `judged`,
  `web`, `work`, `mcp`, and `review`, plus the bundled Starlark batteries `ask`, `skills`,
  and `ttsr-rules`. Keep the boot gate on this complete inventory so a missing battery
  cannot become a silent product variant.
- Keep `dalgona-batteries` as a set of extension constructors. Each constructor must
  return registration records without file, network, process, credential, inference,
  or child-session work; effects begin only after the host starts a session.
- Register compiled batteries through the public Rust extension API. Evaluate bundled
  Starlark batteries with the same loader used for user plugins and mark their origin
  as bundled; a private loader would bypass validation and grants.
- Keep one registration name per battery and one owner per config section, status kind,
  docs page, tool, command, scheme, and hook. Let the shared runtime reject collisions.
- Keep all Dalgona behavior available through the same `Host`, `Agent`, TUI, print,
  JSON, ACP, RPC, router, A2A, and remote-client surfaces as dal. Do not bind a battery
  to one terminal or process.
- Keep the Dalgona manual under `dalgona://` and register it through the shared docs
  door. Do not reach into dal's `dalgon://` registry or copy its pages.

## Configuration and defaults

- Apply configuration in this order: dal code defaults, embedded Dalgona defaults,
  user `config.toml`, then in-memory CLI flags. Let a later layer replace an earlier
  value by key path.
- Replace `plugins`, `disabled_batteries`, and `experimental_batteries` as whole values;
  merging these lists creates entries that the user did not select.
- Decode all Dalgona and battery tables strictly. Reject unknown keys and invalid ranges
  before a session starts; do not keep legacy aliases or silently ignore misspellings.
- Keep one embedded `defaults.toml` as the source of product defaults. Keep symbol search
  enabled, use `hashline` as the default edit style, enable the quality guard, and keep
  the sandbox off unless the public product contract changes.
- Use `search_symbols` as the one switch for symbol search and AST-aware editing. Do not
  restore `symbol_search`, `ast_edit`, `[search]`, or `[patch]` compatibility keys.
- Let users disable batteries only through the product's declared lists. Do not branch
  inside a battery on an unowned product-level setting.

## Effect and authority boundaries

- Use typed extension services for files, processes, network, environment, inference,
  children, jobs, turns, sidecars, questions, notifications, and MCP unless the battery
  is the declared adapter for that effect. Keep MCP HTTP and stdio transport handles
  inside the MCP client modules; do not retain a raw host, journal, or provider handle
  in battery state.
- Declare the exact service set of each battery. Treat a declaration as a permission
  request, and preserve the core runtime's deny-by-default grants for bundled Starlark
  and user code.
- Keep built-in Rust access limited to the capability-free operations the runtime marks
  for built-ins. Do not use built-in origin to bypass a grant for process, network,
  environment, MCP, or another privileged service.
- Validate untrusted tool arguments, config, server replies, HTML, Starlark values, and
  stored extension records at their boundary. Keep typed errors and preserve their
  causes.
- Keep every battery task session-owned and cancellable. On session close, stop timers,
  child sessions, processes, network work, and background jobs, then emit no later wake
  or status update.
- Keep queues, bodies, prompts, replies, status payloads, replay rings, and concurrent
  work bounded. Apply backpressure instead of dropping state-changing events.

## Orchestration

- Use one orchestration owner task per session with one bounded mailbox. Let that task
  own all mutable orchestration state; do not add a task, lock, or map that can race a
  goal, monitor, report, timer, or child event.
- Use the core `Scope`, `agents`, `jobs`, `turn`, `sidecar`, `run`, and `ask` services.
  Do not create another child launcher, process runner, mailbox, budget, wake counter,
  result ring, or cancellation tree.
- Keep child admission bounded and FIFO. Preserve parent ownership, depth limits,
  per-scope budgets, error policy, cascade cancellation, usage roll-up, and the core
  restrictions on child tools and answerers.
- Use the journal-backed mailbox for agent-to-agent messages. Preserve capacity,
  per-pair FIFO order, delivery modes, cursors, and durable replay; process-local
  channels are not a substitute.
- Give one injection arbiter sole ownership of automatic reminders and wakes. Order its
  sources by declared priority, enforce the byte budget, and commit or release each
  source exactly once.
- Preserve the core limit of 20 consecutive wake-started turns without a user prompt.
  Journal each wake attempt and surface the typed limit error; do not reset the count in
  battery memory.
- Treat child and job reports as claims. Rebuild the promised scope, inspect changed
  files, and run the checks before accepting a report as proof.
- Isolate parallel write work in task worktrees. Restrict artifacts to the task's
  session subtree, record retained output explicitly, and do not initialize submodules
  or let a task escape its assigned workspace.

## Goals, monitors, and work state

- Keep goal state durable and explicit. Route create, update, continuation, abort, and
  read operations through one goal state machine and its `goal.json` representation;
  chat text is not goal state.
- Keep monitoring separate from turn creation. Coalesce and rate-limit job activity,
  publish bounded status, and ask the arbiter to wake only after the quiet predicate and
  readiness rules hold.
- Rewrite only recognized sleep-wait process calls into monitored jobs. Preserve other
  commands byte for byte; a heuristic rewrite can change user intent.
- Store plans and todos as durable extension records on the current journal leaf. Fold
  those records for views and goal queries; do not create a second database or sidecar
  copy of the list.
- Validate the complete todo transition before appending it. Keep identifiers stable,
  terminal states final, and plan approval on the shared request broker and approval
  ladder.
- Keep one open plan-selection request per session. Cancellation and dismissal must
  remain failures, not successful approval.

## History and quality

- Keep the compactor order `remote`, `history`, `summary`. Let history decline when its
  model, image, source, or budget contract cannot be met so the local text summary can
  run.
- Preserve exact text behind every history image. Store source chunks and image blobs
  durably, expose the source through `letter://`, and never emit an image without a
  readable source.
- Use catalog capabilities and complete billing profiles to select image handling. Do
  not infer behavior from model-name substrings.
- Keep the base guard engine in dal and its opinionated policies in Dalgona. Enable the
  guard through defaults, feed it staged edits and streamed output, and keep it
  advisory except for deterministic invalid content.
- Report growth deltas, absolute metrics, metric deltas, and current bests from the same
  measured snapshot. Do not recompute displayed values through a separate path.
- Keep quality detectors bounded and deterministic. Register report-only detectors as
  rules through the shared TTSR door; do not add an independent stream watcher or retry
  loop for the same signal.
- Apply codemods only through the shared parser, patch transaction, preview, and
  approval path. A quality offer must not write a file directly.

## Judge-fed behavior and rules

- Use the one dal judge handle and its session gate. Do not add a Dalgona judge model,
  queue, budget, credential probe, ledger, or provider path.
- Keep `auto`, `on`, and `off` resolution fixed for a session. When the judge is off,
  skip judge-fed work without changing base behavior.
- Deduplicate judge-fed reminders through one admission function before spending judge
  budget. Keep the five feature labels stable so ledger and limit accounting remain
  attributable.
- Keep judge-fed hooks within the shared deadline and cancellation budget. Treat their
  output as advice; do not let an observer mutate journal state or tool arguments.
- Load bundled rule packs through the same Markdown and Starlark rule parser as other
  rules. Enforce the shared file, count, pattern, memory, and retry budgets at load time.
- Keep rule fires independent of the front end. Journal first, emit one structured
  update, and render once in the controlling client. Report-only fires must stay silent
  in human views while remaining countable on the wire.
- Keep bundled skill packs as data-only Starlark and Markdown. Do not add services,
  tools, network access, or a second skill resolver to a skill pack.

## Ask, web, MCP, and review

- Raise rich questions through the shared `ask` service and request broker. Preserve
  strict answer shapes and exactly-once answers. Resolve immediately to the fail-closed
  default when no controller can answer; print, JSON, and child sessions are never
  answerers.
- Keep web tools read-only and bounded. Route environment and HTTP access through the
  declared services, allow only configured methods and media types, cap redirects and
  response bytes, and isolate HTML-to-Markdown conversion behind one function.
- Keep MCP limited to tools. Support the native protocol revision and the declared
  legacy handshake only; do not add resources, prompts, sampling, roots, logging,
  subscriptions, or a listener stream without a product decision.
- Require both the extension grant and the normal approval ladder before an MCP mapped
  call reaches a server. Publish mapped tools only at a turn boundary and remove them
  when their declaring generation or session ends.
- Keep MCP transport state per session and per declared server. Bound messages, tool
  counts, output, restarts, and concurrency. Store tokens at mode 0600 and never include
  them in config, logs, errors, prompts, or journals.
- Keep review read-only over `git status` and `git diff`. Use the shared run and direct
  inference services, append one durable review record before returning, and do not
  create a production child reviewer or call the judge question API.

## Testing

- Test each battery through its public extension surface with the scripted provider,
  temporary data roots, in-memory clients, local HTTP servers, and real journal folds.
  Do not use a path dependency or private planning file in a gate.
- Test cross-battery behavior at the Dalgona gates. Include cancellation, restart,
  disabled batteries, strict config, grant denial, budget limits, quiet waits, remote
  clients, and deterministic replay.
- Keep the boot, orchestration, batteries, and release gates independent. A failure must
  name its criterion and retain enough deterministic output for reproduction.
