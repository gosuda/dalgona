# dalgona engineering contract

Maintain this workspace as the opinionated product built on dal. Apply the repository
contract first, then use this file for Dalgona-specific composition and batteries.

## Composition

- Keep the `dalgona` crate as the composition root. Let `product()` register defaults,
  compiled batteries, docs, and binary aliases. Do not put battery behavior in the entry
  crate.
- Register the eleven batteries `orchestration`, `history`, `quality`, `judged`, `web`,
  `work`, `mcp`, `review`, `ask`, `skills`, and `ttsr-rules` as Rust extensions. Keep the
  boot gate on this complete inventory. This rule prevents a missing battery from
  becoming a silent product variant.
- Keep `dalgona-batteries` as a set of extension constructors. Each constructor must
  return registration records without file, network, process, credential, inference,
  or child-session work. Effects begin only after the host starts a session.
- Register every battery through the public Rust extension API and mark its origin as
  bundled. Do not embed Starlark sources in the product. A private registration path
  would bypass validation and grants.
- Keep one registration name per battery and one owner per config section, status kind,
  docs page, tool, command, scheme, and hook. Let the shared runtime reject collisions.
- Keep all Dalgona behavior available through the same `Host`, `Agent`, TUI, print,
  JSON, ACP, RPC, router, A2A, and remote-client surfaces as dal. Do not bind a battery
  to one terminal or process.
- Keep the Dalgona manual under `dalgona://` and register it through the shared docs
  door. Do not reach into dal's `dal://` registry or copy its pages.

## Configuration and defaults

- Apply configuration in this order: dal code defaults, embedded Dalgona defaults,
  user `config.toml`, then in-memory CLI flags. Let a later layer replace an earlier
  value by key path.
- Replace `plugins`, `disabled_batteries`, and `experimental_batteries` as whole values.
  Merging these lists creates entries that the user did not select.
- Decode all Dalgona and battery tables strictly. Reject unknown keys and invalid ranges
  before a session starts. Do not keep legacy aliases or silently ignore misspellings.
- Keep one embedded `defaults.toml` as the source of product defaults. Keep symbol search
  enabled. Use `hashline` as the default edit style. Enable the quality guard. Keep the
  sandbox off unless the public product contract changes.
- Use `search_symbols` as the one switch for symbol search and AST-aware editing. Do not
  restore `symbol_search`, `ast_edit`, `[search]`, or `[patch]` compatibility keys.
- Let users disable batteries only through the product's declared lists. Do not branch
  inside a battery on an unowned product-level setting.

## Effect and authority boundaries

- Use typed extension services for files, processes, network, environment, inference,
  children, jobs, turns, sidecars, questions, notifications, and MCP. A battery may use
  another path only when it is the declared adapter for that effect. Keep the transport
  handles for MCP over HTTP and stdio inside the MCP client modules. Do not retain a raw
  host, journal, or provider handle in battery state.
- Declare the exact service set of each battery. Treat a declaration as a permission
  request. Preserve the core runtime's deny-by-default grants for bundled batteries and
  user code.
- Keep built-in Rust access limited to the capability-free operations that the runtime
  marks for built-ins. Do not use built-in origin to bypass a grant for process, network,
  environment, MCP, or another privileged service.
- Validate untrusted tool arguments, config, server replies, HTML, Starlark values, and
  stored extension records at their boundary. Keep typed errors and preserve their
  causes.
- Keep every battery task session-owned and cancellable. When a session closes, stop
  timers, child sessions, processes, network work, and background jobs. Then emit no
  later wake or status update.
- Keep queues, bodies, prompts, replies, status payloads, replay rings, and concurrent
  work bounded. Apply backpressure instead of dropping state-changing events.

## Orchestration

- Use one orchestration owner task per session with one bounded mailbox. Let that task
  own all mutable orchestration state. Do not add a task, lock, or map that can race a
  goal, monitor, report, timer, or child event.
- Use the core `Scope`, `agents`, `jobs`, `turn`, `sidecar`, `run`, and `ask` services.
  Do not create another child launcher, process runner, mailbox, budget, wake counter,
  result ring, or cancellation tree.
- Keep child admission bounded and FIFO. Preserve parent ownership, depth limits,
  per-scope budgets, error policy, cascade cancellation, usage roll-up, and the core
  restrictions on child tools and answerers.
- Use the journal-backed mailbox for agent-to-agent messages. Preserve capacity,
  per-pair FIFO order, delivery modes, cursors, and durable replay. Process-local
  channels are not a substitute.
- Give one injection arbiter sole ownership of automatic reminders and wakes. Order its
  sources by declared priority. Enforce the byte budget. Commit or release each source
  exactly once.
- Preserve the core limit of 20 consecutive wake-started turns without a user prompt.
  Journal each wake attempt and surface the typed limit error. Do not reset the count in
  battery memory.
- Treat child and job reports as claims. Accept a report as proof only after you rebuild
  the promised scope, inspect the changed files, and run the checks.
- Isolate parallel write work in task worktrees. Restrict artifacts to the task's
  session subtree. Record retained output explicitly. Do not initialize submodules. Do
  not let a task escape its assigned workspace.

## Goals, monitors, and work state

- Keep goal state durable and explicit. Route create, update, continuation, abort, and
  read operations through one goal state machine and its `goal.json` representation.
  Chat text is not goal state.
- Keep monitoring separate from turn creation. Coalesce and rate-limit job activity.
  Publish bounded status. Ask the arbiter to wake only after the quiet predicate and
  readiness rules hold.
- Rewrite only recognized sleep-wait process calls into monitored jobs. Preserve other
  commands byte for byte. A heuristic rewrite can change user intent.
- Store plans and todos as durable extension records on the current journal leaf. Fold
  those records for views and goal queries. Do not create a second database or sidecar
  copy of the list.
- Validate the complete todo transition before appending it. Keep identifiers stable and
  terminal states final. Keep plan approval on the shared request broker and approval
  ladder.
- Keep one open plan-selection request per session. Cancellation and dismissal must
  remain failures. They must not become successful approval.

## History and quality

- Keep the compactor order `remote`, `history`, `summary`. Let history decline when its
  model, image, source, or budget contract cannot be met. Then the local text summary
  can run.
- Preserve exact text behind every history image. Store source chunks and image blobs
  durably. Expose the source through `letter://`. Never emit an image without a readable
  source.
- Use catalog capabilities and complete billing profiles to select image handling. Do
  not infer behavior from model-name substrings.
- Keep the base guard engine in dal and its opinionated policies in Dalgona. Enable the
  guard through defaults. Feed it staged edits and streamed output. Keep it advisory
  except for deterministic invalid content.
- Report growth deltas, absolute metrics, metric deltas, and current bests from the same
  measured snapshot. Do not recompute displayed values through a separate path.
- Keep quality detectors bounded and deterministic. Register report-only detectors as
  rules through the shared TTSR door. Do not add an independent stream watcher or retry
  loop for the same signal.
- Apply codemods only through the shared parser, patch transaction, preview, and
  approval path. A quality offer must not write a file directly.

## Judge-fed behavior and rules

- Use the one dal judge handle and its session gate. Do not add a Dalgona judge model,
  queue, budget, credential probe, ledger, or provider path.
- Keep `auto`, `on`, and `off` resolution fixed for a session. When the judge is off,
  skip judge-fed work without changing base behavior.
- Deduplicate judge-fed reminders through one admission function before spending judge
  budget. Keep the five feature labels stable. This keeps ledger and limit accounting
  attributable.
- Keep judge-fed hooks within the shared deadline and cancellation budget. Treat their
  output as advice. Do not let an observer mutate journal state or tool arguments.
- Register the rule records of each bundled rule pack in Rust and select packs through
  `[rule_sets]`. Take each reminder text from a compiled-in Markdown asset. Enforce the
  shared file, count, pattern, memory, and retry budgets at load time.
- Keep rule fires independent of the front end. Journal first. Emit one structured
  update. Render once in the controlling client. Report-only fires must stay silent in
  human views while remaining countable on the wire.
- Keep bundled skill packs as data-only Rust registrations over compiled-in Markdown. Do
  not add services, tools, network access, or a second skill resolver to a skill pack.

## Ask, web, MCP, and review

- Raise rich questions through the shared `ask` service and request broker. Preserve
  strict answer shapes and exactly-once answers. Resolve immediately to the fail-closed
  default when no controller can answer. Print, JSON, and child sessions are never
  answerers.
- Keep web tools read-only and bounded. Route environment and HTTP access through the
  declared services. Allow only configured methods and media types. Cap redirects and
  response bytes. Isolate HTML-to-Markdown conversion behind one function.
- Keep MCP limited to tools. Support the native protocol revision and the declared
  legacy handshake only. Do not add resources, prompts, sampling, roots, logging,
  subscriptions, or a listener stream without a product decision.
- Require both the extension grant and the normal approval ladder before an MCP mapped
  call reaches a server. Publish mapped tools only at a turn boundary. Remove them when
  their declaring generation or session ends.
- Keep MCP transport state per session and per declared server. Bound messages, tool
  counts, output, restarts, and concurrency. Store tokens at mode 0600. Never include
  tokens in config, logs, errors, prompts, or journals.
- Keep review read-only over `git status` and `git diff`. Use the shared run and direct
  inference services. Append one durable review record before returning. Do not create
  a production child reviewer or call the judge question API.

## Testing

- Test each battery through its public extension surface with the scripted provider,
  temporary data roots, in-memory clients, local HTTP servers, and real journal folds.
  Do not use a path dependency or private planning file in a gate.
- Test cross-battery behavior at the Dalgona gates. Include cancellation, restart,
  disabled batteries, strict config, grant denial, budget limits, quiet waits, remote
  clients, and deterministic replay.
- Keep the boot, orchestration, batteries, and release gates independent. A failure must
  name its criterion. It must retain enough deterministic output for reproduction.
