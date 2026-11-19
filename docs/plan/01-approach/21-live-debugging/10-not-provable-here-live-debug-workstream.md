#### Not provable here (live-debug workstream)

What cannot be proven here:

- Real provider wire formats, OAuth and device-code login against a real provider: needs credentials.
- `dalgona service install` starting at login: needs reboot.
- `dalgona codex install|status` and the app-server under the real app: needs the Codex desktop app.
- `--remote`, `cloud up|down|pull` and remote runners: needs a cloud service.
- Windows pinned writes, ConPTY and the macOS peer check: need another host.
- The sandbox-unavailable notice (W23) on a host where Landlock works: needs another host.
- Nerd glyph switch from kitty's font reply (Design record, Icons): needs another host (kitty with a Nerd Font).
- Clipboard image paste through the OS clipboard: needs a desktop session.
- How real terminals display notifications.
- Whether a real decision model judges well.

The report marks each with the matching `Not proven` status.

