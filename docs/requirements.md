# Product requirements (maintainer spec)

This page is the maintainer spec, not a user guide. To use dal, start with [Quickstart](quickstart.md). To learn why dal has this shape, read [Philosophy](philosophy.md).

Every requirement in this document is a mandatory obligation that dal and dalgona must satisfy. Unmet obligations stand as requirements to implement and verify rather than claims of completed proof.

## Platform and architecture

dal must be a complete and fully usable coding agent platform on its own.
dal must maintain a minimal expandable core with no private interior.
Every capability beyond the core loop must register through the public extension door.
The primary binary must be dalgon.
dal and dl must be identical aliases.
dalgona must extend dal through the public extension door.
It must supply opinionated batteries.
The repository and all workspace packages must inherit the Sustainable Use License Version 1.0 with licensor metaphorics.
Prebuilt binaries for Linux, macOS, and Windows on x86_64 and aarch64 must attach to release assets.

## Seams and protocols

In process, the host-agent seam must provide five operations over one snapshot: read, change stream, command path, request-response, and blob read.
Across processes, dal must serve a JSON-RPC protocol over standard input/output and local sockets that matches the in-process seam.
dal serve must bind to loopback by default without authentication.
Public endpoints must bind all interfaces and require a token.
Headless JSON mode must emit one prompt result on success or one JSON-RPC error object on failure, without streaming handshakes.
dal must support inline terminal, fullscreen terminal user interface, headless print, headless JSON, ACP, RPC, router, and A2A interfaces.
The ACP server must support protocolVersion 1 and protocolVersion 2.
The A2A interface must support version 1.0 over JSON-RPC and HTTP+JSON when started with the --a2a flag.

## Extension and plugin system

dal must evaluate plugins from Starlark scripts at load time with no compilation step.
dal must load user plugins only from the user data directory under plugins.
Workspace directories must never load plugins.
Plugins must declare required capabilities in a deny-by-default manifest.
The host must grant only user-approved capabilities.
Plugins must be able to register tools, slash commands, skills, rules, and prompt sections through the extension door.
First-party extensions must register documentation pages through the shared extension builder.
Plugins must hook into lifecycle events with declared result types: tool calls, user input, turn boundaries, and session lifecycle.
A plugin load failure must halt startup.
It must report the file, line, and column.
Slash command /reload must evaluate fresh plugins without damage to live state if evaluation fails.
One extension software development kit must serve dal and dalgona.
A plugin that loads in dalgon must load in dalgona.
dal must document a conversion workflow to port external TypeScript extensions to Starlark plugins.

## Session journal and state

dal must store sessions as append-only journals that sync to disk on each change.
A record must count as saved only after fsync.
Session histories must form trees.
Resuming with -r or continuing with -c must preserve tree leaves and branches.
Slash commands /tree, /fork, and /clone must inspect and branch session histories.
Slash command /export must write a standalone session transcript file.
Context compaction must run automatically at 85 percent of the context window by default.
It must run remote compaction before local summarization.
Long or stable text may project as image letters.
The exact source text must remain retrievable at letter://<id>.

## Tools and approval

Every mode must provide the read, search, patch, exec, and eval tools.
Eval-first and eval-only modes must also reach plugin tools and prompt sections marked eval_only.
The eval tool must run Starlark only.
An operation denied outside eval must be denied inside eval.
eval must not be treated as a sandbox.
The patch tool must support multiple edit styles.
The default is anchor in dal and hashline in dalgona.
The patch edit style must follow the active model per request when configured in the per-model table.
The approval ladder must follow --approval ask|edits|all.
By default, dal asks before each patch and each exec.
When no person can answer an approval prompt, dal must deny the operation and record the reason.
Read-only tool calls must batch in parallel.
Mutating patch and exec calls must run one at a time in request order.
Long foreground commands must detach into background jobs to preserve interactive control.
Tool execution may carry scoped command prefix grants valid for the active job.
An optional sandbox must guard home files outside allowed paths, with no network restriction.

## Code intelligence and syntax operations

Symbol search must use Tree-sitter to extract definitions from on-disk parses across supported programming languages.
Search mode symbol must list matching definitions, outline single files, and reveal single matches with def tags.
The patch tool must support definition replacement and relative insertion by symbol name and def tag.
Hashline block edits must use the N* operator to address outermost syntactic blocks when symbol search is enabled.
dal must not include a language server protocol client.
Code intelligence must consist of bounded search, syntax walks, and structural patch.

## Prompt construction and provider integration

The system prompt must assemble from AGENTS.md files, SYSTEM.md, active skills, rules, and plugin sections.
TTSR stream rules must inspect model output streams, match patterns, interrupt generation, inject rules, and retry.
dal must connect to four provider families: OpenAI Chat, OpenAI Responses, OpenAI Codex, and Anthropic.
Session commands /model and /thinking must change the model and reasoning effort during a session.
Thinking levels must include off, minimal, low, medium, high, xhigh, and max.
Luna Reserve must operate on the Codex provider as an opt-in model.
It must never be a default model.
Provider and protocol replies must decode tolerantly to support upstream schema evolution.

## Terminal user interface

Text measurement must match terminal drawing cell widths.
It must never split multi-byte or wide characters.
The terminal user interface must support inline scrollback and alternate-screen fullscreen modes.
The terminal user interface must render native diagrams for D2, Nomnoml, DOT, and Mermaid formats.
Terminal themes must provide sufficient contrast to satisfy accessibility contrast standards.

## Self-documentation system

One resolver must serve documentation over immutable pages under dal:// and dalgona:// schemes.
Documentation URIs must follow the grammar scheme://page with lowercase segments and no traversal syntax.
Every page must be valid UTF-8 Markdown with a title on line one.
It must stay under 49152 bytes and 1500 lines.
Slash command /docs and read tool scheme reads must render manual pages without line numbers or edit footers.
Failed page lookups must provide nearest suggestions within an edit distance of two.

## Guard and batteries

The core must ship a lean judge component with configurable model roles, typed judgment, and auto|on|off gating.
In dal the judge must feed judged rules and eval batching.
dalgona must enable all judge-fed capabilities by default.
dalgona must ship batteries for web search and fetch, plan and todo, a rich questionnaire tool, an MCP client, and a review loop.
dalgona must enable the anti-complexity and anti-slop guard by default.
dal must keep the guard disabled by default.
The guard must measure growth deltas, absolute metrics, metric deltas, and current bests.
It must block only deterministic invalid content.

## Concurrency and resource bounds

dal must maintain structured concurrency with explicit ownership, race-free state transitions, and no leaked threads or processes.
Concurrency invariants and lock ordering must be verifiable under deterministic permutation tests.
Configuration parsing must deny unknown fields and report the exact offending line in dal.toml.
Under high load, the system must bound memory growth, manage open file descriptors, and maintain responsive keypress echo latency.

## Next

- [Philosophy](philosophy.md): read why dal has one extension door.
- [Tools, modes, and approval](tools.md): read how the tool and approval rules behave today.
