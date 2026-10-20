# dal JSON-RPC protocol reference

The dal protocol lets another process drive dal. Use it to build a GUI, an editor integration, or a test harness on the same operations that the terminal uses. Run `dalgon rpc` to serve it. This page lists the methods, the rules for each method group, and the rules for updates.

Each frame is one JSON-RPC 2.0 message on one line, ended by a line feed. A frame holds at most 16777216 bytes. A larger frame ends the connection. Frames run over stdio or a local socket. `initialize` negotiates capabilities. The protocol version is 1. A client that can answer approval requests lists `approval` in its `initialize` capabilities. A client that can answer extension questions lists `question`. A session with no such client attached denies approvals. It gives questions their default answer at once.

| method | params | result | capability |
|---|---|---|---|
| initialize | protocolVersion, capabilities | ready | none |
| protocol/schema | none | schema | none |
| commands/list | none | commands | none |
| session/list | limit, cursor, search | sessions | sessions |
| session/open | ref | session | sessions |
| session/close | sessionId | ok | sessions |
| session/view | sessionId, limit, before | view | sessions |
| session/subscribe | sessionId, gen, after | stream | sessions |
| session/unsubscribe | sessionId | ok | sessions |
| session/submit | sessionId, command | reply | sessions |
| session/answer | sessionId, requestId, answer | ok | sessions |
| blob/read | sessionId, blobId | bytes | blobs |
| models/list | none | models | models |
| docs/read | uri | text | docs |
| host/subscribe | none | stream | host.updates |
| host/unsubscribe | none | ok | host.updates |
| auth/status | none | providers | auth |
| auth/login | provider, method, apiKey | pending, then ok | auth |
| auth/cancel | loginId | cancelled | auth |
| auth/logout | provider | removed | auth |

## Start a connection

Run `dalgon rpc`. Send `initialize` as the first request. A request before `initialize` fails with `-32006` and the message `initialize must be the first request`. A second `initialize` fails with `-32600`. Only `protocol/schema` may come first.

Write this line to standard input:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"capabilities":["sessions"],"clientInfo":{"name":"demo"}}}
```

The server writes one line back. It names the protocol version, the server, the capabilities it accepted, and your client id. In this reply, `<version>` is the server version and `<n>` is a connection number:

```text
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"serverInfo":{"name":"dal","version":"<version>"},"capabilities":["sessions"],"clientId":"demo#<n>"}}
```

The reply lists only the capabilities that the server supports and that you asked for. It never echoes `approval` or `question`. A method that needs a capability you did not get fails with `-32007` and the message `capability "<name>" is not enabled on this connection`.

## Request rules

Requests need exactly one answer. Blobs are content-addressed. Error codes follow JSON-RPC. `experimental/` methods may change.

## Authentication methods

An `auth/login` with `method` `browser` or `device` answers `pending` with the `loginId` and `url` (and `userCode` for `device`) as soon as the provider flow reports one. When the flow ends, the host publishes one `login_finished` host update naming the attempt's `loginId`. `method` `api_key` takes the key in `apiKey`. It answers `ok` after the key is stored. `auth/cancel` takes the `loginId` of a pending login. It cancels the attempt. It answers whether one was still pending. `auth/status` lists each provider as `ready`, `not_configured`, or `expired`. `auth/logout` without `provider` removes every stored credential. A `provider` that is present but not a string is refused with `-32602`. The host removes nothing.

## Error handling

Seven methods decode their params strictly: `auth/login`, `auth/cancel`, `auth/logout`, `session/list`, `session/view`, `session/subscribe`, and `docs/read`. In them, an omitted optional member selects its default. A present member of the wrong type (or non-object params) is refused with `-32602`. Members that take no default stay required (`provider` and `method` for `auth/login`, `sessionId` wherever used). Invalid parameter errors format their message as `invalid params for <method>: <detail>` with code `-32602`. ACP `session/list` accepts null `cwd` and `cursor` filters.

## Session commands

`session/submit` takes every command type the `Command` decoder knows. An unknown `type` is refused with `-32602`. `session/submit` refuses an `export` command whose path is absolute or climbs with `..`. It answers `-32602` and writes no file. Relative paths inside the workspace and the default target stay valid.

## Updates

Updates carry `seq` and `gen`. Clients resync on gaps.

## Extension status

An extension that registers a status kind reports changes as `ext_status` updates with `ext`, `state` (`busy` or `quiet`), and optional `text`. For example: `{"type":"ext_status","ext":"focus","state":"busy","text":"indexing"}` and `{"type":"ext_status","ext":"focus","state":"quiet"}`. The host polls each kind on a short fixed interval. It sends an update only when the state or text changes. A quiet extension with no text is the starting state. It sends nothing until it has been busy. A fresh wire subscription has no status seed. It receives changes as they occur. An embedded client reads the current busy or text-bearing statuses with `Agent::ext_status`. ACP clients receive the same change as a `_dal/notice` with `kind` `status` and the text `<ext>: <text>`, `<ext>: busy`, or `<ext>: quiet`. A2A streams carry it as the `dal.status` metadata of a status update. The router sends nothing. `dalgon --json` writes its result line only after every status kind is quiet.

## Shutdown

A server that is stopping answers each new request with `-32009` (`server_draining`). It gives running requests one second to finish. Then it closes the connection.

## Model rows

Each `models/list` row carries `id`, `provider`, `name`, and an optional `contextWindow`. `provider` is the label of the row's API family. Every chat and responses provider reads `openai`. It does not name a configured provider. Two rows can therefore share `id` and `provider`. Each row also carries `route`, the typed route that selects exactly that row, and an optional `reference`:

| Row | `route` | `reference` |
|---|---|---|
| API model with a known provider | `{"kind":"api","family":..,"model":..,"provider":..}` | `provider/model` |
| API model without a known provider | the same without `provider` | absent |
| Synthetic or harness model | `{"kind":"synthetic","id":..}` or `{"kind":"harness","id":..}` | the id |

A client that selects a row submits its `route` in `set_model`. It submits nothing it rebuilt from `id` or `provider`. A server that predates `route` and `reference` omits them. A client then keeps its older selection by name. That name cannot tell same-family providers apart. A member that is present but malformed is an error, not an absent member. `auth/status` rows and the protocol version 1 are unchanged.

## Assistant entries

Assistant entries carry one `model` identifier. Their optional `route` member identifies the route that produced the response:

| Value | Meaning |
|---|---|
| `{"api":"openai_chat"}` | An API route, with its API family. The other family values are `openai_responses`, `openai_codex`, and `anthropic`. |
| `"synthetic"` | A named synthetic model. |
| `"harness"` | A built-in harness mode. |

The route describes the existing `model` identifier. Its API family is independent of the entry's top-level `api` member, which describes the provider wire format. New responses include `route`. An absent member means the route is unknown, not that it is an API route. The decoder rejects `null`, unknown route kinds, and model identifiers that do not fit the declared synthetic or harness kind.

`protocol/schema` includes this optional, non-null member. Protocol version 1 and the format-1 encoding of assistant entries remain unchanged. Turn-ID reservations use journal format 2. See [durable turn identities](sessions.md#durable-turn-identities).

## Next

- [Build a client on the dal protocol](integrate.md): connect a program through `dalgon rpc`.
- [Embed dal in a Rust program](sdk.md): call the same operations in process.
- [Command line](cli.md): start `dalgon rpc`, `dalgon acp`, or `dalgon serve`.
