# Run the dalgon model router and A2A server

`dalgon serve` starts an HTTP server on your machine. The server routes OpenAI-style and Anthropic-style requests to the models you signed in to. It can also run the full dal agent behind the same address. With `--a2a`, it serves the agent-to-agent surface too.

Use it to give a script, a tool, or a second machine one endpoint for your dal sign-in. The server binds to loopback and needs no token by default. `--public` listens wider and requires a token from `dalgon serve token`.

## Start the server

```sh
dalgon serve
```

The server prints one line to standard output and keeps running:

```text
dalgon serve listening on http://127.0.0.1:7437 (loopback, no token)
```

The port is `serve.port` in `dal.toml`. Its default is 7437. Pass `--port PORT` or `--bind ADDR` to override the address for one run. Stop the server with Ctrl+C. To change the port for every run, set it in `dal.toml`:

```toml
[serve]
port = 8080
```

## Send a request

The `model` field picks the target. The server accepts these ids:

| id | target |
|---|---|
| `dalgon/normal`, `dalgon/eval-first`, `dalgon/eval-only` | the dal agent in that mode, with its own tools |
| a key of `[aliases]` | the model that the alias names |
| `openai-chat/<model>`, `openai-responses/<model>`, `openai-codex/<model>`, `anthropic/<model>` | that provider, through a pass-through relay |

`GET /v1/models` lists the model ids to use. An unknown id fails with the code `model_not_found` and the text `model "<id>" was not found`.

```sh
curl -s http://127.0.0.1:7437/v1/models
curl -s http://127.0.0.1:7437/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"dalgon/normal","messages":[{"role":"user","content":"Say hello."}]}'
```

These routes exist:

| route | use |
|---|---|
| `GET /v1/models` | list model ids |
| `POST /v1/chat/completions` | OpenAI Chat Completions |
| `POST /v1/responses` | OpenAI Responses |
| `POST /v1/messages` | Anthropic Messages |
| `GET /v1/ws` | WebSocket upgrade for protocol clients |
| `GET /codex/ws` | WebSocket upgrade for Codex clients |

Every POST needs `Content-Type: application/json`. Otherwise the server answers 415 with `Content-Type must be application/json`. A request with an `Origin` header gets 403 `browser requests are not allowed`, except a WebSocket upgrade. A server without `--public` also refuses a `Host` header that is not a loopback name. It answers 403 with `Host header "<host>" is not a loopback name`.

## Continue a session

A `dalgon/...` id runs the dal harness. The harness refuses client tools, tool choices, and tool history. A request continues a session in one of three ways:

- an `x-dal-session` header that holds a session id
- a `previous_response_id` in a Responses request
- a history that matches an earlier reply

Anything else opens a new session in the directory where `dalgon serve` runs. An unknown session id fails with `session_not_found`.

The router cannot ask anyone. It denies an approval it needs at once, and an extension question takes its default answer. `[serve] approval` sets the approval policy for actions that arrive through the server. It takes `ask`, `edits`, or `all`. Use `edits` or `all` only on a server that nobody else can reach.

```toml
[serve]
approval = "edits"
```

## Listen beyond loopback

Create a token. The command prints the token once and stores it in `serve.token` in the data root.

```sh
dalgon serve token
```

Start the server with `--public`. It then listens on all interfaces and requires the token.

```sh
dalgon serve --public
```

The server prints a warning to standard error and a listen line to standard output:

```text
dalgon: warning: --public serves 0.0.0.0:7437 over plain HTTP; the serve token crosses the network in plain text.
dalgon serve listening on http://0.0.0.0:7437 (all interfaces, token from <token-file>)
```

Clients send the token as `Authorization: Bearer <token>` or as `x-api-key: <token>`. The server answers 401 `missing_token` when the token is absent. It answers 401 `invalid_token` with `the bearer token does not match serve.token` when the token is wrong.

```sh
curl -s -H "Authorization: Bearer $TOKEN" http://HOST:7437/v1/models
```

Pass `--token-file FILE` to read the token from another file. To create the token at that path, put the flag before `token`: `dalgon serve --token-file FILE token`. A second `dalgon serve token` fails with `dalgon: serve.token already exists: <path>` while the file exists. Add `--force` to replace the token. Every client must then use the new one.

The WebSocket address works with the dal terminal. Run `dalgon --connect HOST:7437 --token-file FILE` on the other machine. `--connect` takes `host:port`, a `ws://` or `wss://` URL, or an absolute socket path. dalgon sends the token only to a non-loopback WebSocket address. `--connect` needs the interactive terminal. With `--print` or `--json`, it fails with `dalgon: --connect requires the interactive TUI`.

## Serve A2A

```sh
dalgon serve --a2a
```

The detail in the listen line then ends with `, a2a on`, for example `(loopback, no token, a2a on)`. The server mounts these routes. Without `--a2a`, each one answers 404 with `no route for <method> <path>`.

| route | use |
|---|---|
| `GET /.well-known/agent-card.json` | the agent card |
| `POST /a2a` | A2A JSON-RPC |
| `POST /a2a/v1/message:send` | send a message |
| `POST /a2a/v1/message:stream` | send a message and stream the reply |
| `GET /a2a/v1/tasks/<id>` | read a task |
| `POST /a2a/v1/tasks/<id>:cancel` | cancel a task |

On a public server, the agent card needs no token. Every other route does.

## Fix a start error

Each start error prints two lines: what failed, then the fix.

| first line | cause |
|---|---|
| `dalgon: cannot bind 127.0.0.1:7437: <message>` | Another process holds the port. Stop it, or pass `--port` with a free port. |
| `dalgon: serve --bind <address> is not a loopback address: it needs a token` | Pass `--public`, or bind 127.0.0.1. |
| `dalgon: serve --public needs a token: <path> does not exist` | Run `dalgon serve token`. |
| `dalgon: serve.token is open to other users: <path> has mode <mode>` | Run `chmod 600 <path>`, or run `dalgon serve token --force`. |
| `dalgon: [aliases] key "<alias>" shadows a dal mode id` | Rename the alias. |

## Next

- [Protocol](protocol.md) lists the methods that the WebSocket routes carry.
- [Build a client on the dal protocol](integrate.md) shows a worked client over `dalgon rpc`.
- [Settings in dal.toml](config.md) lists the `[serve]` keys.
