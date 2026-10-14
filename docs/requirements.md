# Requirements dal must meet

Every requirement in this document is a mandatory obligation that dal and dalgona must satisfy. Unmet obligations stand as requirements to implement and verify rather than claims of completed proof.

## Platform and Architecture

dal must be a complete and fully usable coding agent platform on its own.
dal must maintain a minimal expandable core with no private interior.
Every capability beyond the core loop must register through the public extension door.
The primary binary must be dalgon, and dal and dl must be identical aliases.
dalgona must extend dal through the public extension door and supply opinionated batteries.
The repository and all workspace packages must inherit the Sustainable Use License Version 1.0 with licensor metaphorics.
Prebuilt binaries for Linux, macOS, and Windows on x86_64 and aarch64 must attach to release assets.

## Seams and Protocols

In process, the host-agent seam must provide five operations over one snapshot: read, change stream, command path, request-response, and blob read.
Across processes, dal must serve a JSON-RPC protocol over standard input/output and local sockets matching the in-process seam.
dal serve must bind to loopback by default without authentication, while public endpoints must bind all interfaces and require a token.
Headless JSON mode must emit one prompt result on success or one JSON-RPC error object on failure, without streaming handshakes.
dal must support inline terminal, fullscreen terminal user interface, headless print, headless JSON, ACP, RPC, router, and A2A interfaces.
The ACP server must support protocolVersion 1 and protocolVersion 2.
The A2A interface must support version 1.0 over JSON-RPC and HTTP+JSON when started with the --a2a flag.

## Extension and Plugin System

Plugins must be evaluated from Starlark scripts at load time with no compilation step.
dal must load user plugins exclusively from the user data directory under plugins; workspace directories must never load plugins.
Plugins must declare required capabilities in a deny-by-default manifest, and the host must grant only user-approved capabilities.
Plugins must be able to register tools, slash commands, skills, rules, and prompt sections through the extension door.
First-party extensions must register documentation pages through the shared extension builder.
Plugins must hook into lifecycle events with declared result types: tool calls, user input, turn boundaries, and session lifecycle.
A plugin load failure must halt startup and report the file, line, and column.
Slash command /reload must evaluate fresh plugins without tearing live state if evaluation fails.
One extension software development kit must serve dal and dalgona, so that a plugin that loads in dalgon loads in dalgona.
dal must document a conversion workflow to port external TypeScript extensions to Starlark plugins.

## Session Journal and State

Sessions must be stored as append-only journals synced to disk on each change, and a record must count as saved only after fsync.
Session histories must form trees, and resuming with -r or continuing with -c must preserve tree leaves and branches.
Slash commands /tree, /fork, and /clone must inspect and branch session histories.
Slash command /export must write a standalone session transcript file.
Context compaction must run automatically at 85 percent of the context window by default, running remote compaction before local summarization.
Long or stable text may project as image letters while the exact source text must remain retrievable at letter://<id>.

## Tools and Approval

Normal mode must provide read, search, patch, and exec tools.
Eval-first and eval-only modes must gate tools through Starlark evaluation.
The eval tool must run Starlark only; an operation denied outside eval must be denied inside eval, and eval must not be treated as a sandbox.
The patch tool must support multiple edit styles, defaulting to anchor in dal and hashline in dalgona.
The patch edit style must follow the active model per request when configured in the per-model table.
The approval ladder must follow --approval ask|edits|all, asking before patch and exec execution by default.
When no person can answer an approval prompt, dal must deny the operation and record the reason.
Read-only tool calls must batch in parallel, while mutating patch and exec calls must run one at a time in request order.
Long foreground commands must detach into background jobs to preserve interactive control.
Tool execution may carry scoped command prefix grants valid for the active job.
An optional sandbox must guard home files outside allowed paths, with no network restriction.

## Code Intelligence and Syntax Operations

Symbol search must extract definitions from on-disk parses using Tree-sitter across supported programming languages.
Search mode symbol must list matching definitions, outline single files, and reveal single matches with def tags.
The patch tool must support definition replacement and relative insertion by symbol name and def tag.
Hashline block edits must use the N* operator to address outermost syntactic blocks when symbol search is enabled.
dal must not include a language server protocol client; code intelligence must consist of bounded search, syntax walks, and structural patch.

## Prompt Construction and Provider Integration

The system prompt must assemble from AGENTS.md files, SYSTEM.md, active skills, rules, and plugin sections.
TTSR stream rules must inspect model output streams, match patterns, interrupt generation, inject rules, and retry.
dal must connect to four provider families: OpenAI Chat, OpenAI Responses, OpenAI Codex, and Anthropic.
Session commands /model and /thinking must change the model and reasoning effort during a session.
Thinking levels must include off, minimal, low, medium, high, xhigh, and max.
Luna Reserve must operate on the Codex provider as an opt-in model and must never be a default model.
Provider and protocol replies must decode tolerantly to support upstream schema evolution.

## Terminal User Interface

Text measurement must match terminal drawing cell widths and must never split multi-byte or wide characters.
The terminal user interface must support inline scrollback and alternate-screen fullscreen modes.
The terminal user interface must render native diagrams for D2, Nomnoml, DOT, and Mermaid formats.
Terminal themes must provide sufficient contrast to satisfy accessibility contrast standards.

## Self-Documentation System

Documentation must be served by one resolver over immutable pages under dal:// and dalgona:// schemes.
Documentation URIs must follow the grammar scheme://page with lowercase segments and no traversal syntax.
Every page must be valid UTF-8 Markdown with a title on line one, staying under 49152 bytes and 1500 lines.
Slash command /docs and read tool scheme reads must render manual pages without line numbers or edit footers.
Failed page lookups must provide nearest suggestions within an edit distance of two.

## Guard and Batteries

The core must ship a lean judge component with configurable model roles, typed judgment, and auto|on|off gating.
In dal the judge must feed judged rules and eval batching; dalgona must enable all judge-fed capabilities by default.
dalgona must ship batteries for web search and fetch, plan and todo, a rich questionnaire tool, an MCP client, and a review loop.
dalgona must enable the anti-complexity and anti-slop guard by default, while dal must keep the guard disabled by default.
The guard must measure growth deltas, absolute metrics, metric deltas, and current bests, blocking only deterministic invalid content.

## Concurrency and Resource Bounds

dal must maintain structured concurrency with explicit ownership, race-free state transitions, and no leaked threads or processes.
Concurrency invariants and lock ordering must be verifiable under deterministic permutation tests.
Configuration parsing must deny unknown fields and report the exact offending line in dal.toml.
Under high load, the system must bound memory growth, manage open file descriptors, and maintain responsive keypress echo latency.
