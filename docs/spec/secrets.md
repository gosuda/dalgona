# Secret masking

Credentials and configured secret-pattern matches MUST be masked before tool output or prompts enter the journal or a model request. Values shorter than eight bytes MUST NOT be masked. Workspace MCP approval prompts MUST show only env keys as `KEY=<redacted>` with no raw values in the summary, reason, commands, or RPC log (see `mcp.md`).

Masking MUST use reversible placeholders. Environment values use `{{secret:<ENV_NAME>}}`. The environment source includes every variable whose name ends in `_API_KEY`, `_TOKEN`, `_SECRET`, or `_PASSWORD`.

The primary credential value for a provider uses `{{secret:auth:<provider>}}`. Additional OAuth access or refresh values MUST use distinct provider-qualified placeholders so unmasking cannot interchange them.

Pattern matches use `{{secret:pattern<i>:<n>}}`. The pattern index identifies the configured RE2 pattern. The value index MUST remain stable for the same pattern and value across calls and process restarts. New distinct values MUST receive new indexes.

The masker MUST persist pattern hits in `secrets.json` before `Mask` returns masked text. If persistence fails, `Mask` MUST return `secret masking failed: pattern state was not persisted`. It MUST NOT return unrecoverable placeholders or the original secret text. `MaskError` MUST report the failure. Unsaved entries MUST remain dirty so a later `Mask` call retries the write. The file MUST have mode 0600. The loader MUST reject symlinks, non-regular files, and group or other permission bits. Windows MUST check the regular-file requirement. Writers MUST serialize on `<home>/auth.lock`, MUST reload `secrets.json` under that lock, MUST merge mappings by secret value against the reloaded set with colliding placeholders reassigned, MUST allocate new placeholders against the merged set, and MUST refresh the in-memory map after the write. `Unmask` MUST refresh mappings from disk before substituting placeholders.

The masker MUST select the longest matching value at each input position. Equal-length matches MUST have deterministic tie-breaking. Replacement MUST process the original input once and MUST NOT scan generated placeholders for more secrets.

`Unmask` MUST restore known placeholders. Unknown placeholders MUST remain unchanged. The exec and patch tools MUST unmask with `UnmaskStrict` immediately before execution or resolution and MUST fail the call without writing or running when a placeholder with masker grammar has no known value.

Read snapshots and edit pre-images MUST be masked before they enter the blob store. Project instructions (`AGENTS.md` and `CLAUDE.md` text stored for a session) MUST be masked before they enter the blob store. Snapshot metadata MUST name the masked bytes. Restores MUST unmask to the original bytes before use, so recovery and planning keep working with no raw secrets in the journal. A restore that encounters a secret placeholder with masker grammar and no known value MUST fail closed: recovery MUST abort with an error without writing placeholder bytes, and snapshot opens MUST abort with an error without populating the store. Text that does not match the masker grammar (for example `{{secret:example}}`) MUST NOT fail strict restores and MUST remain unchanged. Unknown placeholders MUST remain unchanged on display paths only. Masker grammar is `{{secret:<ENV_NAME>}}` where the name ends in `_API_KEY`, `_TOKEN`, `_SECRET`, or `_PASSWORD`, `{{secret:auth:<provider>}}` with optional `:access` or `:refresh`, and `{{secret:pattern<i>:<n>}}` where both indexes are decimal.

When `MaskError` reports a failure, the host MUST fail the submit or tool call and MUST NOT journal or send the failure sentence. Prompts MUST be rejected before journaling. Tool output MUST fail the call with `OK=false` or return an error, never with the failure sentence as output.

`auth.json` MUST be a regular file with mode 0600. The exact refusal error is `auth.json must be a regular file with mode 0600`. Windows MUST check only the regular-file requirement. Writes MUST use a unique temporary file, mode 0600, and an atomic rename. Unknown provider entries and their unknown fields MUST survive a load and write cycle without modification.

Credential and secret files MUST use streaming `jsontext` encoders and decoders. Reflection-based JSON encoding MUST NOT be used.
