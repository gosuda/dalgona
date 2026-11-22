# Documentation

Use [the project README](../README.md) for installation and the product command surface. Use [CONTRIBUTE.md](../CONTRIBUTE.md) for changes and [REVIEW.md](../REVIEW.md) for review rules. Use [DESIGN.md](../DESIGN.md) for the visual system.

The specifications define required behavior. They do not certify that each command or integration has passed its acceptance checks. The decisions state invariants, scope, and enforcement. RFC 2119 keywords such as MUST and SHOULD carry their normative meaning in these documents.

Shared documents belong to `dal/` and stay intact there. In particular, Starlark replay and the combined Starlark specification remain shared contracts. Their battery-only sections are labeled and link to the battery tree.

## Terms

Use one name for each thing in user-facing text and new specifications.

- Thread: one conversation with its journal, turns, and settings. Write thread, not session. The storage names `session.created` and `sessions/` stay.
- Thread grant: an approval that lets later calls of one command or tool run in the same thread. The journal names it `grant:<prefix>`. Write thread grant, not session grant.
- Follow-up: a prompt queued while a turn runs; it starts the next turn. `followup` is only the `user.message` kind.
- Thinking and reasoning: interfaces show thinking; the wire, journal, and config field is `reasoning`.
- Subagent: a thread that a tool call starts under another thread. `child.spawned` and `parent` are its journal names.
- Interrupt: the user stops the running turn, which ends with status `interrupted`. `cancel` is only an approval decision.
- Mode: the per-thread approval mode, the sandbox mode, and the tool mode set in configuration. For remote runners, name thread boxes and the daemon in a box as [runners.md](dalgona/spec/runners.md) and [config.md](dal/spec/config.md) define them.
- Generation: one built search index of a workspace, in a directory named for its build start. `CURRENT` names the published generation.
- Manifest: the file in a generation that lists each eligible file with its size, its times, and whether the index holds it.
- Trigram: a three-byte sequence that the search index uses to name candidate files.

## Specifications

### Shared core (dal)

- [Event journal](dal/spec/journal.md): record framing, append order, mirror repair, backups, and writer locks.
- [Agent events](dal/spec/events.md): event fields, validation, and folded state.
- [Recovery](dal/spec/recovery.md): pending-call classification after a halt or restart.
- [Local runner](dal/spec/runner.md): detached shims, durable output, process identity, and signals.
- [Exec server](dal/spec/exec-server.md): remote process and filesystem wire, bootstrap, and version checks.
- [Harness sandbox](dal/spec/sandbox.md): write confinement and platform fallbacks.
- [Search](dal/spec/search.md): size ceiling and regular-file scans.
- [Edit styles](dal/spec/edit-styles.md): parser, resolver, writer, and refusal contracts.
- [Rewrite](dal/spec/rewrite.md): structural replacement by tree-sitter capture, its bounds, and its refusals.
- [Edit review](dal/spec/edit-review.md): judge each edit on one diff, turn reviews, and diff hunks.
- [Failure records](dal/spec/failures.md): bounded masked records, capture, privacy classes, and refusals.
- [Hashline profiles](dal/spec/hashline/README.md): shared, Light, Enhanced, conformance, and review documents. This imported set distinguishes proposal requirements from its bounded model-check evidence.
- [Hashline shared contract](dal/spec/hashline/hashline-common-v1.md): syntax, edit evidence, mapping, lifecycle, and authorization.
- [Hashline Light](dal/spec/hashline/hashline-light-v1.md): snapshot-bound edits without a prior-observation requirement.
- [Hashline Enhanced](dal/spec/hashline/hashline-enhanced-v1.md): prior coverage, delivery cutoffs, and strict recovery.
- [Hashline conformance](dal/spec/hashline/hashline-conformance-v1.md): acceptance matrix and implementation requirements.
- [Hashline review](dal/spec/hashline/hashline-review.md): findings, amendments, rejected alternatives, and source-line map.
- [Hashline model checker](dal/spec/hashline/verify_model.py): bounded range and gap identity checks, not a filesystem or patch engine.
- [Hashline model results](dal/spec/hashline/verification.json): recorded range, gap, and regression check results.
- [Configuration](dal/spec/config.md): product homes, precedence, keys, and atomic settings writes.
- [Providers](dal/spec/providers.md): request mapping, streams, role presets, and provider-specific behavior.
- [OAuth flows](dal/spec/oauth.md): authorization, device login, token exchange, and refresh.
- [Secret masking](dal/spec/secrets.md): reversible placeholders and credential file rules.
- [Approvals](dal/spec/approvals.md): gate order, modes, decider inputs, and recovery.
- [Static approval rules](dal/spec/approval-rules.md): shell classification and thread grants.
- [Decision clients](dal/spec/decide.md): typed questions, adapter formats, and time limits.
- [Advisor](dal/spec/advisor.md): review cadence, note delivery, isolation, and recovery.
- [Magic keywords](dal/spec/keywords.md): user-prose matching, notices, and required tools.
- [Starlark](dal/spec/starlark.md): durable runs, effect replay, the eval tool, workflows, and plugins.
- [Ask tool](dal/spec/ask.md): question arguments, cleaning, and result text for the user-question tool.
- [Protocol](dal/spec/protocol.md): Codex-compatible JSON-RPC framing and backpressure.
- [TUI](dal/spec/tui.md): terminal behavior, slash commands, and status.
- [Live debugging](dal/spec/live-debug.md): local scripted-server contract, setup and cleanup runbook, and manual terminal and browser acceptance matrix. It distinguishes planned probes from recorded live or test evidence.

### Batteries (dalgona)

- [Sandbox services](dalgona/spec/sandbox-services.md): Modal and Sandbox0 lifecycle and persistence.
- [Remote runners](dalgona/spec/runners.md): thread boxes, fully cloud mode, pull, and box cleanup.
- [Ratchet](dalgona/spec/ratchet.md): held-out eval flows, approvals, and the keep, revert or rerun gate.
- [Web UI server](dalgona/spec/web.md): built assets, access tokens, TLS, and the daemon websocket relay.
- [Native desktop client](dalgona/spec/native.md): the complete Go and Gio desktop implementation contract and source map.
- [Native architecture](dalgona/spec/native-architecture.md): shared connection lifecycle, package boundaries, state ownership, and reconnect.
- [Native protocol](dalgona/spec/native-protocol.md): required capabilities, history, images, authentication, workspaces, and terminals.
- [Native design](dalgona/spec/native-design.md): web design mapping, desktop tokens, focus, and keyboard behavior.
- [Native components](dalgona/spec/native-components.md): every web component and the additional desktop workflows.
- [Native documents](dalgona/spec/native-documents.md): selectable text, code, figures, math, images, diffs, and terminal rendering.
- [Native platforms](dalgona/spec/native-platform.md): OS adapters, accessibility, build isolation, packaging, and installation.
- [Native verification](dalgona/spec/native-verification.md): complete release gates, behavioral fixtures, and implementation order.
- [Daemon service](dalgona/spec/daemon.md): socket, startup, and user-service lifecycle.
- [Doctor](dalgona/spec/doctor.md): the setup check command, its rows, and its offline and online modes.
- [Codex app integration](dalgona/spec/codex-app.md): desktop override installation and restoration.
- [MCP client](dalgona/spec/mcp.md): tool discovery, calls, and transports.
- [Discovery](dalgona/spec/discovery.md): opt-in imports of instructions, skills, and server config.
- [herdr integration](dalgona/spec/herdr.md): pane reports and subagent panes.
- [Daltoki coordinator service](dalgona/spec/daltoki.md): GitHub event triage and fixes through delegation graphs.
- [Delegation graph](dalgona/spec/delegation-graph.md): strict JSON graph value with parsing and validation.
- [Shared GitHub client](dalgona/spec/github.md): one client with bot and reporter policies.
- [Consented failure reporter](dalgona/spec/report.md): bounded opt-in failure reports.

### Clients and integrations

- [Continuous integration](dal/spec/ci.md): verification groups, native platforms, shared assets, and box image builds.
- [Cloudflare deployment](../deploy/cloudflare/README.md): worker tokens, image release, digest pins, and box cleanup.

`just spec-style` checks sentence length, semicolons, em dashes, and lower-case `should` in `docs/dal/spec` and `docs/dalgona/spec`.

## Plan

- [Completion plan](plan/README.md): the plan that stabilized and completed both products, stored as one fragment per section. It records intended work and certifies nothing.

## Decisions

Each decision file states its Invariant, Scope, and Enforcement. It MAY end with `## Rejected alternatives`; each entry names one alternative and the one reason it lost. A replaced decision MUST stay in its product directory: add `Superseded by [<title>](<file>.md) on <YYYY-MM-DD>.` under its title, keep its text, and move its entry to the end of its product list with the suffix `(superseded)`. Never delete a decision file.

### Shared core (dal)

- [Two products](dal/decisions/two-products.md): shared core, separate homes, and battery boundaries.
- [Codex protocol as core](dal/decisions/codex-protocol-as-core.md): one client protocol and inward dependency direction.
- [Journal authority and mirror](dal/decisions/journal-authority-and-mirror.md): JSONL authority, mirror repair, and damage quarantine.
- [Starlark replay](dal/decisions/starlark-replay.md): pinned source and recorded effects.
- [Syntax package](dal/decisions/syntax-package.md): one tree-sitter boundary, parse bounds, and measured costs.
- [Minimal harness sandbox](dal/decisions/minimal-harness-sandbox.md): accidental-write confinement, not hostile-code isolation.
- [Automated approval](dal/decisions/automated-approval.md): static refusals and automatic decisions.
- [Model presets](dal/decisions/model-presets.md): native identifiers, fallback, and selection evidence.
- [Copilot provider](dal/decisions/copilot-provider.md): device login, token exchange, and unverified editor identity.
- [Model prices](dal/decisions/model-prices.md): price rows, rounding, and the cost that derives from them.
- [Secret masking](dal/decisions/secret-masking.md): raw-secret exclusion and tool-boundary restoration.
- [Advisor](dal/decisions/advisor.md): optional review without approval authority.
- [Dependencies](dal/decisions/dependencies.md): codecs, import restrictions, and pins.
- [Connect wire](dal/decisions/connect-wire.md): one framing and field-helper package for the Connect providers.
- [Edit styles](dal/decisions/edit-styles.md): anchor default, explicit hashline profiles, and no symbol locators.
- [Edit review](dal/decisions/edit-review.md): optional review of edit diffs without approval authority.
- [Ask tool](dal/decisions/ask-tool.md): model questions answered by the user through the client wire.
- [llmstub test support](dal/decisions/llmstub-test-support.md): scripted chat-completions server for tests and live sessions.
- [Search index](dal/decisions/search-index.md): the grep index, its trust rule, and its rejected engines.
- [RPC file search](dal/decisions/rpc-file-search.md): workspace file listing through search tool walker.
- [RPC thread search](dal/decisions/rpc-thread-search.md): thread text search through the journal mirror index.
- [Shell argument parser](dal/decisions/shell-argument-parser.md): one neutral command splitter and token reader for core packages.
- [Journal authority and mirror](dal/decisions/journal-authority-and-mirror.md): JSONL as event authority with a repairable mirror.
- [Syntax package](dal/decisions/syntax-package.md): tree-sitter access confined to one package.

### Batteries (dalgona)

- [Native Gio client](dalgona/decisions/native-client.md): direct Go client integration, complete native scope, and web design inheritance.
- [Sandbox services](dalgona/decisions/sandbox-services.md): remote execution through the Service seam.
- [Ratchet](dalgona/decisions/ratchet.md): host-owned eval gate in the dalgona battery.
- [Sandbox services](dalgona/decisions/sandbox-services.md): the Service seam, box lifecycle, and persistence contracts.
- [Web toolchain](dalgona/decisions/web-toolchain.md): TypeScript compatibility with the Svelte tools.
