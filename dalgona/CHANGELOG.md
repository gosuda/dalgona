# Changelog

## Unreleased
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

## 0.1.0 (2026-09-26)

- Initial release of the headless loop, terminal, compiled batteries, wire surfaces, and manual.
