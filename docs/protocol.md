# The dal protocol

Framing over stdio or a local socket; `initialize` negotiates capabilities. The protocol version is 1.

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

Updates carry `seq` and `gen`; clients resync on gaps. An extension that registers a status kind reports changes as `ext_status` updates with `ext`, `state` (`busy` or `quiet`), and optional `text`; for example, `{"type":"ext_status","ext":"focus","state":"busy","text":"indexing"}` and `{"type":"ext_status","ext":"focus","state":"quiet"}`. The host polls each kind on a short fixed interval and sends an update only when the state or text changes. A quiet extension with no text is the starting state and sends nothing until it has been busy. A fresh wire subscription has no status seed and receives changes as they occur; an embedded client reads the current busy or text-bearing statuses with `Agent::ext_status`. ACP clients receive the same change as a `_dal/notice` with `kind` `status` and the text `<ext>: <text>`, `<ext>: busy`, or `<ext>: quiet`. A2A streams carry it as the `dal.status` metadata of a status update. The router sends nothing. `dalgon --json` writes its result line only after every status kind is quiet. Requests need exactly one answer. Blobs are content-addressed. Error codes follow JSON-RPC. A server that is stopping answers each new request with `-32009` (`server_draining`), gives running requests one second to finish, and then closes the connection. `experimental/` methods may change.
