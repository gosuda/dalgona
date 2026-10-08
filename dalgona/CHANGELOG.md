# Changelog

## Unreleased

- Add opt-in fenced-diagram rendering to TUI transcript rows and ask previews; Dalgona asks models for supported diagram fences when enabled.
- Assemble Dalgona's batteries as bundled Rust extensions through dal's public extension API.
- Stop the review tool at the round cap and list the findings still open; running `/review` after the cap starts a new session through the new `restart` argument, which never discards rounds in progress.

## 0.1.0 (2026-09-26)

- Initial release of the headless loop, terminal, compiled batteries, wire surfaces, and manual.
