# Secret masking

Credentials and configured secret-pattern matches MUST be masked before tool output or prompts enter the journal or a model request. Values shorter than eight bytes MUST NOT be masked.

Masking MUST use reversible placeholders. Environment values use `{{secret:<ENV_NAME>}}`. The environment source includes every variable whose name ends in `_API_KEY`, `_TOKEN`, `_SECRET`, or `_PASSWORD`.

The primary credential value for a provider uses `{{secret:auth:<provider>}}`. Additional OAuth access or refresh values MUST use distinct provider-qualified placeholders so unmasking cannot interchange them.

Pattern matches use `{{secret:pattern<i>:<n>}}`. The pattern index identifies the configured RE2 pattern. The value index MUST remain stable for the same pattern and value across calls and process restarts. New distinct values MUST receive new indexes.

The masker MUST persist pattern hits in `secrets.json` before `Mask` returns masked text. If persistence fails, `Mask` MUST return `secret masking failed: pattern state was not persisted`. It MUST NOT return unrecoverable placeholders or the original secret text. `MaskError` MUST report the failure. Unsaved entries MUST remain dirty so a later `Mask` call retries the write. The file MUST have mode 0600. The loader MUST reject symlinks, non-regular files, and group or other permission bits. Windows MUST check the regular-file requirement.

The masker MUST select the longest matching value at each input position. Equal-length matches MUST have deterministic tie-breaking. Replacement MUST process the original input once and MUST NOT scan generated placeholders for more secrets.

`Unmask` MUST restore known placeholders. Unknown placeholders MUST remain unchanged. The exec and patch tools MUST unmask immediately before execution or resolution.

`auth.json` MUST be a regular file with mode 0600. The exact refusal error is `auth.json must be a regular file with mode 0600`. Windows MUST check only the regular-file requirement. Writes MUST use a unique temporary file, mode 0600, and an atomic rename. Unknown provider entries and their unknown fields MUST survive a load and write cycle without modification.

Credential and secret files MUST use streaming `jsontext` encoders and decoders. Reflection-based JSON encoding MUST NOT be used.
