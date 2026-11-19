## Critical files

- `internal/tui/model.go`: today's whole terminal client. The terminal workstream splits it and replaces its render path with the dock.
- `internal/rpc/serve_track.go`: the tracker that turns journal events into wire items. Most protocol, image-input, ask and edit-review steps add a case here.
- `internal/agent/tools.go`: the life of a tool call (`worker.started`, completion, masking). The post-image blob, the edit review and the ask tool hook in here, and `internal/agent/turn.go:154` is where the turn review runs.
- `web/src/routes/+page.svelte`: today's whole browser page. The web workstream moves its state into a session and its parts into components.
- `docs/spec/protocol.md`: the client contract. Each wire change adds its sentence here in the same commit.
- `internal/tool/search/engine.go`: the walk and the line matcher of the search tool. The index adds one skip to it, and the symbol modes reuse its walk, ignore rules and footers.
- `internal/script/mode.go`: the tool modes. The eval-default workstream changes the direct list and deletes `DefaultUses`.

