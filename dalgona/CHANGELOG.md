# Changelog

## Unreleased

- Keep compaction images within the request size limit: history compaction now subtracts the bytes of the images that stay in the request from its image byte budget, so it draws fewer images, or declines, instead of building a request the provider rejects.
- Add opt-in fenced-diagram rendering to TUI transcript rows and ask previews; Dalgona asks models for supported diagram fences when enabled.
- Assemble Dalgona's batteries as bundled Rust extensions through dal's public extension API.
- Stop the review tool at the round cap and list the findings still open; running `/review` after the cap starts a new session through the new `restart` argument, which never discards rounds in progress.
- Fix MCP servers that use OAuth when several calls get a 401 at once: one token refresh serves all of them. A refresh that fails for a temporary reason, such as a network error or a server error, no longer forces a new login, and a token from a new login replaces the old token for every connection.
- Fix `/abort` and session end with orchestration children: every queued or running child and its descendants are cancelled, and a failed cancel or list no longer stops the sweep. `/abort` reports each failure with the child id, and `agents cancel` tries every named id and reports each refusal.

## 0.1.0 (2026-09-26)

- Initial release of the headless loop, terminal, compiled batteries, wire surfaces, and manual.
