# Changelog

## Unreleased

- Draw compacted history as images: for a model that reads images, compaction now emits the older journal as labeled PNG images with exact-text `letter://` records, reuses stored letters across compactions, and keeps stored text as text. The text summary still runs whenever the image path declines.
- Run the history compactor before the text summary: the compaction chain is now `remote`, `history`, `summary`, so an image-reading model gets images instead of a summary whenever the image caps allow it.
- Fix `read dalgona://<page>`: the model can now read the Dalgona manual, not only the `dalgona docs` command.
- Include an MCP stdio server's exit status and bounded stderr excerpt when it crashes during a call.
- Fix the review battery: `/review` can now read the git status and diff of a session, so it reports findings instead of failing with a workspace-size error.
- Fix the history compaction notice for a model that reads images when this host cannot store them: it now says `history: this host cannot commit image parts.` instead of `history: the journal source is unavailable.`, and the text summary still runs.
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
