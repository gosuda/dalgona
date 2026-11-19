#### Schemas of the web workstream

This workstream adds no wire message, journal event, tool argument or config key. The prerequisites of this workstream lists every wire member and config key it reads, with the shape it assumes. It adds two browser-side contracts.

Browser storage key `dalgona.theme` (`localStorage`)
- Type: string. Enum: `auto | auto-soft | day | soft-day | night | soft-night`. Any other value reads as `auto`.
- Written by `applyTheme`. Read once at startup, before the first `config/read`.
- Example: `localStorage.setItem("dalgona.theme", "soft-night")`. The matching config key is `theme`: enum string `auto | auto-soft | day | soft-day | night | soft-night`, default `auto`, example `theme = "auto-soft"`.

Browser check result (`web/checks/*.js`)
- Each file is one script evaluated in the page. Its final expression is a Promise of `{ ok: boolean, failures: string[] }`.
- `failures` holds at most 50 entries, each `"<selector or check>: <observed> (want <expected>)"`.
- Example: `{ "ok": false, "failures": ["button.primary: 22x40 (want >= 24x24)"] }`.

