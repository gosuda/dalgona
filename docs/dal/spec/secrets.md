# Secret masking

Credentials, configured secret-pattern matches, and built-in secret matches MUST be masked before tool output or prompts enter the journal or a model request. Values shorter than eight bytes MUST NOT be masked. Workspace MCP approval prompts MUST show only env keys as `KEY=<redacted>` with no raw values in the summary, reason, commands, or RPC log (see [mcp.md](../../dalgona/spec/mcp.md)).

Masking MUST use reversible placeholders. Environment values use `{{secret:<ENV_NAME>}}`. The environment source includes every variable whose name ends in `_API_KEY`, `_TOKEN`, `_SECRET`, or `_PASSWORD`.

Login-shell capture uses `auth.SecretEnvironmentName` to exclude secret variables from its cached delta.
The snapshot MUST NOT persist, journal, spill, or broadcast capture bytes.
The capture buffer is cleared after parsing or failure.
Existing daemon variables remain available to child processes.
Filtering the delta does not remove credentials from the daemon environment.

The primary credential value for a provider uses `{{secret:auth:<provider>}}`. Additional OAuth access or refresh values MUST use distinct provider-qualified placeholders so unmasking cannot interchange them.

Pattern matches use `{{secret:pattern<i>:<n>}}`. The pattern index identifies the configured RE2 pattern. The value index MUST remain stable for the same pattern and value across calls and process restarts. New distinct values MUST receive new indexes.

Built-in matches use `{{secret:builtin<i>:<n>}}`. The index `<i>` identifies the built-in rule. The value index `<n>` follows the same rules as a pattern value index.

The built-in rules use the indexes in the table below. A new rule MUST take the next free index. An existing index MUST NOT change.

| Index | Match |
|---|---|
| 0 | `AKIA` and 16 uppercase letters or digits |
| 1 | `ghp_`, `gho_`, `ghu_`, `ghs_` or `ghr_`, and 36 or more letters or digits |
| 2 | `github_pat_` and 22 or more letters, digits or `_` |
| 3 | `xoxb-`, `xoxa-`, `xoxp-`, `xoxr-` or `xoxs-`, and 10 or more letters, digits or `-` |
| 4 | `AIza` and 35 or more letters, digits, `-` or `_` |
| 5 | `sk-ant-`, an optional `api` with two digits and `-`, and 20 or more letters, digits, `_` or `-` |
| 6 | `sk-` and 20 or more letters, digits, `_` or `-` |
| 7 | A PEM private key block from its `BEGIN` line through its `END` line |
| 8 | A JWT with three or five dot-separated segments of 10 or more characters. The first segment starts with `eyJ` |

A built-in match MUST start at a word boundary. A `sk-` match with an interior hyphen and no digit MUST NOT match, as in `sk-machine-learning-library`. The prefixes `sk-proj-`, `sk-svcacct-` and `sk-admin-` MUST match without a digit.

Built-in matches MUST NOT overlap. The scanner MUST order matches by start, then by longer match, then by lower rule index. The masker MUST NOT report a match that lies inside a placeholder with masker grammar. A match equal to an environment or provider credential value MUST keep the credential placeholder and MUST NOT enter `secrets.json`.

The `[secrets]` key `builtin` MUST be a boolean and defaults to `true`. When it is `false`, the masker MUST NOT scan for new built-in matches. The masker MUST still mask built-in values that `secrets.json` already holds.

The masker MUST persist pattern matches and built-in matches in `secrets.json` before `Mask` returns masked text. If persistence fails, `Mask` MUST return `secret masking failed: pattern state was not persisted`. It MUST NOT return unrecoverable placeholders or the original secret text. `MaskError` MUST report the failure. Unsaved entries MUST remain dirty so a later `Mask` call retries the write. The file MUST have mode 0600. The loader MUST reject symlinks, non-regular files, and group or other permission bits. On Windows the loader MUST check the regular-file requirement. The home directory MUST carry a protected DACL that grants access only to the current user, SYSTEM, and Administrators. Writers MUST serialize on `<home>/auth.lock`. Writers MUST reload `secrets.json` under that lock. Writers MUST merge mappings by secret value against the reloaded set. Writers MUST reassign a colliding placeholder during the merge. Writers MUST allocate new placeholders against the merged set. Writers MUST refresh the in-memory map after the write. `Unmask` MUST refresh mappings from disk before substituting placeholders.

If `auth.json` cannot be reread, `Mask` MUST return `secret masking failed: auth.json could not be reread`.

The masker MUST select the longest matching value at each input position. Equal-length matches MUST have deterministic tie-breaking. Replacement MUST process the original input once and MUST NOT scan generated placeholders for more secrets.

`Unmask` MUST restore known placeholders. Unknown placeholders MUST remain unchanged. The exec, patch and rewrite tools MUST unmask with `UnmaskStrict` immediately before execution or resolution. The exec, patch and rewrite tools MUST fail the call without writing or running when a placeholder with masker grammar has no known value. The rewrite tool MUST unmask its pattern and replace arguments only, and MUST NOT unmask a path or a glob. If a placeholder with masker grammar has no known value, `UnmaskStrict` MUST return an error that names the placeholder.

Read snapshots and edit pre-images MUST be masked before they enter the blob store. Project instructions (`AGENTS.md` and `CLAUDE.md` text stored for a thread) MUST be masked before they enter the blob store. Custom keyword notices MUST be masked before they enter the journal or a model request. A notice masking failure MUST fail the submit. Snapshot metadata MUST name the masked bytes. Restores MUST unmask to the original bytes before use, so recovery and planning keep working with no raw secrets in the journal. A restore that encounters a secret placeholder with masker grammar and no known value MUST fail closed. Recovery MUST abort with an error without writing placeholder bytes. Snapshot opens MUST abort with an error without populating the store. Text that does not match the masker grammar (for example `{{secret:example}}`) MUST NOT fail strict restores and MUST remain unchanged. Unknown placeholders MUST remain unchanged on display paths only. The masker grammar includes `{{secret:<ENV_NAME>}}`, where the name ends in `_API_KEY`, `_TOKEN`, `_SECRET`, or `_PASSWORD`. The masker grammar includes `{{secret:auth:<provider>}}`, with an optional `:access` or `:refresh` suffix. The masker grammar includes `{{secret:pattern<i>:<n>}}`, where both indexes are decimal. The masker grammar includes `{{secret:builtin<i>:<n>}}`, where both indexes are decimal. The masker grammar includes `{{secret:keyword:<n>}}`, where `<n>` is decimal.

Image bytes MUST NOT pass through the masker. An image path MUST be masked before it enters the journal. A failed path mask MUST fail the submit that carries the image.

When `MaskError` reports a failure, the host MUST fail the submit or tool call and MUST NOT journal or send the failure sentence. The host MUST reject prompts before journaling. The host MUST fail the tool call with `OK=false` or return an error, and it MUST NOT return the failure sentence as output.

Skipped, denied, and interrupted tool outputs MUST pass the mask before they enter the journal. A mask failure MUST stop the skip or denial event. Interruption MUST record the output `[output masking failed]` and log the failure, because the turn is already ending.

`auth.json` MUST be a regular file with mode 0600. The exact refusal error is `auth.json must be a regular file with mode 0600`. The Windows rules in the `secrets.json` paragraph apply to `auth.json`. Writes MUST use a unique temporary file, mode 0600, and an atomic rename. Unknown provider entries and their unknown fields MUST survive a load and write cycle without modification.

Credential and secret files MUST use streaming `jsontext` encoders and decoders. Reflection-based JSON encoding MUST NOT be used.
