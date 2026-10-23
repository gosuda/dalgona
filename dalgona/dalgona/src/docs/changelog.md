# Changes by version

## Unreleased

- Assemble Dalgona's batteries as bundled Rust extensions through dal's public extension API.
- Canceled queued prompts no longer delay goal continuation. The orchestration battery drops canceled queued input without treating it as a user turn.
- Persist worktree status records (`status.txt`) atomically as sidecar artifacts before worktree removal during open-time salvage, leaving trees and registration in place if persistence fails.

## 0.1.0 (2026-09-26)

- Initial release of the headless loop, terminal, compiled batteries, wire surfaces, and manual.
