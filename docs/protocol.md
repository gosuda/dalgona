# The dal protocol

Framing over stdio or a local socket; `initialize` negotiates capabilities. The protocol version is 1. A client that can answer approval requests lists `approval` in its `initialize` capabilities, and a client that can answer extension questions lists `question`. A session with no such client attached denies approvals and gives questions their default answer at once.

| method | params | result | capability |
|---|---|---|---|
| initialize | hello | ready | core |
| session/list | none | sessions | core |
| session/open | id | session | core |
| session/subscribe | id | stream | core |
| session/close | id | ok | core |
| blob/read | id | bytes | core |
| protocol/schema | none | schema | core |
| docs/read | uri | text | docs |
| auth/status | none | providers | auth |
| auth/login | provider, method, apiKey | pending, then ok | auth |
| auth/cancel | loginId | cancelled | auth |
| auth/logout | provider | removed | auth |

An `auth/login` with `method` `browser` or `device` answers `pending` with the `loginId` and `url` (and `userCode` for `device`) as soon as the provider flow reports one, and publishes one `login_finished` host update when the flow ends; `method` `api_key` takes the key in `apiKey` and answers `ok` after the key is stored. `auth/cancel` takes the `loginId` of a pending login, cancels the attempt, and answers whether one was still pending. `auth/status` lists each provider as `ready`, `not_configured`, or `expired`. `auth/logout` without `provider` removes every stored credential; a `provider` that is present but not a string is refused with `-32602` and removes nothing. For `auth/login`, `auth/cancel`, `auth/logout`, `session/list`, `session/view`, `session/subscribe`, and `docs/read`, an omitted optional member selects its default while a present member of the wrong type (or non-object params) is refused with `-32602`; members that take no default stay required (`provider` and `method` for `auth/login`, `sessionId` wherever used). ACP `session/list` accepts null `cwd` and `cursor` filters. `session/submit` takes every command type the `Command` decoder knows; an unknown `type` is refused with `-32602`. `session/submit` refuses an `export` command whose path is absolute or climbs with `..`, answering `-32602` and writing no file; relative paths inside the workspace and the default target stay valid. Updates carry `seq` and `gen`; clients resync on gaps. An extension that registers a status kind reports changes as `ext_status` updates with `ext`, `state` (`busy` or `quiet`), and optional `text`; for example, `{"type":"ext_status","ext":"focus","state":"busy","text":"indexing"}` and `{"type":"ext_status","ext":"focus","state":"quiet"}`. The host polls each kind on a short fixed interval and sends an update only when the state or text changes. A quiet extension with no text is the starting state and sends nothing until it has been busy. A fresh wire subscription has no status seed and receives changes as they occur; an embedded client reads the current busy or text-bearing statuses with `Agent::ext_status`. ACP clients receive the same change as a `_dal/notice` with `kind` `status` and the text `<ext>: <text>`, `<ext>: busy`, or `<ext>: quiet`. A2A streams carry it as the `dal.status` metadata of a status update. The router sends nothing. `dalgon --json` writes its result line only after every status kind is quiet. Requests need exactly one answer. Blobs are content-addressed. Error codes follow JSON-RPC. A server that is stopping answers each new request with `-32009` (`server_draining`), gives running requests one second to finish, and then closes the connection. `experimental/` methods may change.
