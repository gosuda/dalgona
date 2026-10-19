# Changes by version

## Unreleased

- Assemble Dalgona's batteries as bundled Rust extensions through dal's public extension API.
- Add the `procs` battery: one deferred tool that lists this session's background jobs and greps their retained output. Disable it with `[plugin.procs] enabled = false` or `disabled_batteries`; the page is `dalgona://procs`.
- `procs` list rows now show the command the starter ran (an exec job names its command; a job started without one shows its label), collapsed to one line and cut at 80 characters, and `query` matches the label or the full command, not the truncated display.

## 0.1.0 (2026-09-26)

- Initial release of the headless loop, terminal, compiled batteries, wire surfaces, and manual.
