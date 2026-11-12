# Documentation

Use [the project README](../README.md) for installation and the product command surface. Use [CONTRIBUTE.md](../CONTRIBUTE.md) for changes and [REVIEW.md](../REVIEW.md) for review rules.

The specifications define required behavior. They do not certify that each command or integration has passed its acceptance checks. The decisions state invariants, scope, and enforcement. RFC 2119 keywords such as MUST and SHOULD carry their normative meaning in these documents.

## Terms

Use one name for each thing in user-facing text and new specifications.

- Thread: one conversation with its journal, turns, and settings. Write thread, not session. The storage names `session.created` and `sessions/` stay.
- Thread grant: an approval that lets later calls of one command or tool run in the same thread. The journal names it `grant:<prefix>`. Write thread grant, not session grant.
- Follow-up: a prompt queued while a turn runs; it starts the next turn. `followup` is only the `user.message` kind.
- Thinking and reasoning: interfaces show thinking; the wire, journal, and config field is `reasoning`.
- Subagent: a thread that a tool call starts under another thread. `child.spawned` and `parent` are its journal names.
- Interrupt: the user stops the running turn, which ends with status `interrupted`. `cancel` is only an approval decision.
- Mode: the per-thread approval mode and sandbox mode, and the tool mode set in configuration. Name the remote shapes thread boxes and the daemon in a box.

## Specifications

### State and execution

- [Event journal](spec/journal.md): record framing, append order, mirror repair, backups, and writer locks.
- [Agent events](spec/events.md): event fields, validation, and folded state.
- [Recovery](spec/recovery.md): pending-call classification after a halt or restart.
- [Local runner](spec/runner.md): detached shims, durable output, process identity, and signals.
- [Exec server](spec/exec-server.md): remote process and filesystem wire, bootstrap, and version checks.
- [Harness sandbox](spec/sandbox.md): write confinement and platform fallbacks.
- [Sandbox services](spec/sandbox-services.md): Modal and Sandbox0 lifecycle and persistence.
- [Remote runners](spec/runners.md): thread boxes, fully cloud mode, pull, and box cleanup.
- [Search](spec/search.md): size ceiling and regular-file scans.
- [Edit styles](spec/edit-styles.md): parser, resolver, writer, and refusal contracts.
- [Hashline profiles](spec/hashline/README.md): shared, Light, Enhanced, conformance, and review documents. This imported set distinguishes proposal requirements from its bounded model-check evidence.
- [Hashline shared contract](spec/hashline/hashline-common-v1.md): syntax, edit evidence, mapping, lifecycle, and authorization.
- [Hashline Light](spec/hashline/hashline-light-v1.md): snapshot-bound edits without a prior-observation requirement.
- [Hashline Enhanced](spec/hashline/hashline-enhanced-v1.md): prior coverage, delivery cutoffs, and strict recovery.
- [Hashline conformance](spec/hashline/hashline-conformance-v1.md): acceptance matrix and implementation requirements.
- [Hashline review](spec/hashline/hashline-review.md): findings, amendments, rejected alternatives, and source-line map.
- [Hashline model checker](spec/hashline/verify_model.py): bounded range and gap identity checks, not a filesystem or patch engine.
- [Hashline model results](spec/hashline/verification.json): recorded range, gap, and regression check results.
- [Ratchet](spec/ratchet.md): held-out eval flows, approvals, and the keep, revert or rerun gate.

### Configuration and model behavior

- [Configuration](spec/config.md): product homes, precedence, keys, and atomic settings writes.
- [Providers](spec/providers.md): request mapping, streams, role presets, and provider-specific behavior.
- [OAuth flows](spec/oauth.md): authorization, device login, token exchange, and refresh.
- [Secret masking](spec/secrets.md): reversible placeholders and credential file rules.
- [Approvals](spec/approvals.md): gate order, modes, decider inputs, and recovery.
- [Static approval rules](spec/approval-rules.md): shell classification and thread grants.
- [Decision clients](spec/decide.md): typed questions, adapter formats, and time limits.
- [Advisor](spec/advisor.md): review cadence, note delivery, isolation, and recovery.
- [Magic keywords](spec/keywords.md): user-prose matching, notices, and required tools.
- [Starlark](spec/starlark.md): durable runs, effect replay, the eval tool, workflows, and plugins.
- [Ask tool](spec/ask.md): question arguments, cleaning, and result text for the user-question tool.

### Clients and integrations

- [Protocol](spec/protocol.md): Codex-compatible JSON-RPC framing and backpressure.
- [TUI](spec/tui.md): terminal behavior, slash commands, and status.
- [Web UI server](spec/web.md): built assets, access tokens, TLS, and the daemon websocket relay.
- [Daemon service](spec/daemon.md): socket, startup, and user-service lifecycle.
- [Codex app integration](spec/codex-app.md): desktop override installation and restoration.
- [MCP client](spec/mcp.md): tool discovery, calls, and transports.
- [Discovery](spec/discovery.md): opt-in imports of instructions, skills, and server config.
- [herdr integration](spec/herdr.md): pane reports and subagent panes.

`just spec-style` checks sentence length, semicolons and dashes in `docs/spec`.

## Decisions

Each decision file states its Invariant, Scope, and Enforcement. It MAY end with `## Rejected alternatives`; each entry names one alternative and the one reason it lost. A replaced decision MUST stay in this directory: add `Superseded by [<title>](<file>.md) on <YYYY-MM-DD>.` under its title, keep its text, and move its entry to the end of this list with the suffix `(superseded)`. Never delete a decision file.

- [Two products](decisions/two-products.md): shared core, separate homes, and battery boundaries.
- [Codex protocol as core](decisions/codex-protocol-as-core.md): one client protocol and inward dependency direction.
- [Journal authority and mirror](decisions/journal-authority-and-mirror.md): JSONL authority and verified repair.
- [Starlark replay](decisions/starlark-replay.md): pinned source and recorded effects.
- [Minimal harness sandbox](decisions/minimal-harness-sandbox.md): accidental-write confinement, not hostile-code isolation.
- [Automated approval](decisions/automated-approval.md): static refusals and automatic decisions.
- [Sandbox services](decisions/sandbox-services.md): service seam and persistence choices.
- [Model presets](decisions/model-presets.md): native identifiers, fallback, and selection evidence.
- [Secret masking](decisions/secret-masking.md): raw-secret exclusion and tool-boundary restoration.
- [Advisor](decisions/advisor.md): optional review without approval authority.
- [Dependencies](decisions/dependencies.md): codecs, import restrictions, and pins.
- [Web toolchain](decisions/web-toolchain.md): TypeScript compatibility with the Svelte tools.
- [Connect wire](decisions/connect-wire.md): one framing and field-helper package for the Connect providers.
- [Edit styles](decisions/edit-styles.md): anchor default, explicit hashline profiles, and no symbol locators.
- [Ask tool](decisions/ask-tool.md): model questions answered by the user through the client wire.
- [Ratchet](decisions/ratchet.md): host-owned eval gate in the dalgona battery.
