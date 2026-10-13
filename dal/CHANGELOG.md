# Changelog

## Unreleased

- Add opt-in fenced-diagram rendering for terminal transcript rows and ask previews: the `/settings` picker toggles `tui.diagrams` for the session and a save row persists it to `dal.toml` through the standard product configuration; CLI-backed diagram renders run on background workers with a pending placeholder, and pixel results draw through the terminal's image protocol (kitty, sixel, or iTerm2) instead of a text card.

- Persist image-bearing compaction parts and their extension letters atomically, replay them from the journal, and resolve letter history sources after resume; pass the selected catalog image profile, retained-image count, and carried summary to compactors.
- Accept mapped MCP tool names in registration, dispatch, approval, and replay; send provider-safe aliases to providers and restore internal names on streamed calls.
- Initial release.
- Add extension status consumers. A registered status kind now publishes an `ext_status` update (`busy` or `quiet`, with optional text) only when it changes; `Agent::ext_status`, `Agent::poll_status`, `Agent::is_quiet`, and `Host::is_quiet` read it. The terminal shows busy extensions above the status line when the layout has room, ACP sends a `status` notice, A2A adds `dal.status` metadata, and the router sends nothing. `dalgon --json` writes its line only after every status kind is quiet; print mode waits 3 seconds and exits 1 with `dalgon: extension status is not quiet` if a kind stays busy.
- Breaking: change the plugin API to Plugin and Eval v1. A plugin now publishes one `plugin` value built after `load("@dal/v1", "dal")`. Tools take an `input` schema, handlers are `run(ctx, args)`, each tool, hook, and model lists its operations in `uses`, and the plugin requests host services through `inject`. This replaces `dal.tool(name = ...)`, `dal.prompt_section`, and `run(args, ctx)`. To update a plugin, follow `dal://plugins`.
- Add scripted synthetic model routes to Plugin and Eval v1. The `models` map associates a plugin-local key with `dal.model(id = ..., caps = ..., run = ..., uses = [...])`; `id` is the public `namespace/route` name. `run(ctx, request)` receives the provider-neutral request and can call `ctx.models.infer` or `ctx.models.forward`. Add `infer` to the plugin's `inject` services when models call those operations. A scripted model can forward once per run; the host rejects cycles and synthetic model chains deeper than four routes.
- Add `always_apply`, `scope`, `interrupt_mode`, `repeat_mode`, and `repeat_gap` to `dal.rule`, so a plugin rule can apply every turn or fire only in chosen content, such as `scope = ["tool:exec"]`.
- Change `eval` to take `{code, uses?, data?}`. A cell inherits its operations from `eval.uses`, and `uses = []` makes it pure.
- Remove the `notify` example plugin; its behavior cannot run under the v1 hook limits.
- Fix extension records: `append_record` now writes a journal record before it returns, `records` reads only the calling extension's rows on the current leaf path, and a tool call runs as the extension that registered it, with that extension's `inject` set. Records survive resume and follow `/tree` leaf moves.
