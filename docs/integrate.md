# Build a client on the dal protocol

A dal client talks to the agent over the dal protocol. Each message is one JSON object on one line. `dalgon` offers several ways in. This page builds a GUI client over `dalgon rpc`, then connects an editor and a Codex client.

Use the protocol to put your own UI, editor, or script in front of dal. The [protocol reference](protocol.md) lists every method.

## Choose a way in

| way in | command | frames |
|---|---|---|
| dal JSON-RPC on standard input and output | `dalgon rpc` | one JSON object per line |
| dal JSON-RPC on a local socket | `dalgon rpc --socket [FILE]` | one JSON object per line; `FILE` defaults to the per-user socket |
| Agent Client Protocol, for editors | `dalgon acp` | one JSON-RPC message per line |
| Codex app-server protocol | `dalgon app-server` | one message per line; frames omit `jsonrpc` |
| HTTP router, A2A, and WebSockets | `dalgon serve` | see [Run the dalgon model router and A2A server](serve.md) |

## Try it

Send one `initialize` request to `dalgon rpc`. The server ends when standard input ends.

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | dalgon rpc
```

The server answers with one line. Its `id` is `1`, and `result.protocolVersion` is `1`.

## Build a GUI client

Do these steps in order. Each request is one line on the standard input of `dalgon rpc`. Each reply is one line on its standard output.

1. Spawn `dalgon rpc`. Write one JSON object per line. Read one JSON object per line.
2. Send `initialize` first. List the capabilities you need. The result carries `protocolVersion`, the capabilities the server grants, and your `clientId`. Any other request before `initialize` fails with code `-32006` and `initialize must be the first request`. A second `initialize` fails with `-32600` and `initialize was already called`.

   ```json
   {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"my-gui","version":"0.1.0"},"capabilities":["sessions","approval"]}}
   ```

3. Call `session/list` to show past sessions. `limit` runs from 1 to 500. A page that has more rows carries `nextCursor`.

   ```json
   {"jsonrpc":"2.0","id":2,"method":"session/list","params":{"limit":10}}
   ```

4. Call `session/open` to start or resume one. `workspace` must be an absolute path. The result carries `sessionId`, `gen`, and `view`. Other `ref` types are `resume` with `key` and `workspace`, `continue` with `workspace`, and `ephemeral` with `workspace`.

   ```json
   {"jsonrpc":"2.0","id":3,"method":"session/open","params":{"ref":{"type":"new","workspace":"/home/me/project"}}}
   ```

5. Call `session/subscribe` to receive updates. The reply holds `gen` and `seq`. Then the server sends `session/update` notifications. Each one names its update type in `params.update.type`.

   ```json
   {"jsonrpc":"2.0","id":4,"method":"session/subscribe","params":{"sessionId":"<sessionId>"}}
   ```

6. Call `session/submit` with a `prompt` command to start a turn.

   ```json
   {"jsonrpc":"2.0","id":5,"method":"session/submit","params":{"sessionId":"<sessionId>","command":{"type":"prompt","expect":"idle","content":[{"type":"text","text":"Explain src/main.rs"}]}}}
   ```

7. Render updates as they arrive. A `request_opened` update asks you for an answer. Answer it once with `session/answer`. Use the request `id` as `requestId`. The `answer` is `{"type":"approve"}`, `{"type":"approve_for_session"}`, `{"type":"decline"}`, `{"type":"cancel"}`, or `{"type":"value","value":...}`.

   ```json
   {"jsonrpc":"2.0","id":6,"method":"session/answer","params":{"sessionId":"<sessionId>","requestId":"<requestId>","answer":{"type":"approve"}}}
   ```

8. Cancel a running turn with `session/submit` and a `cancel` command. `turn` is the number of the running turn.

   ```json
   {"jsonrpc":"2.0","id":7,"method":"session/submit","params":{"sessionId":"<sessionId>","command":{"type":"cancel","scope":{"type":"turn","turn":1}}}}
   ```

9. Read a stored blob with `blob/read`. Pass `sessionId` and `blobId`.
10. Reconnect with `session/subscribe`. Pass the `gen` you hold and `after`, the last `seq` you saw. A stale `gen` makes the server send a resync and resume at the head. An `after` beyond the head fails with `invalid params for session/subscribe: after <n> is beyond seq <m>`.
11. Generate your types from `protocol/schema`.

A wrong parameter fails with code `-32602`. The message reads `invalid params for <method>: <detail>`. A relative workspace fails with `invalid params for session/open: workspace must be an absolute path`.

## Client rules

- R1: answer each request exactly once.
- R2: never invent missing updates. Resync on gaps.
- R3: cancel cleanly. Do not abandon turns.
- R4: keep tokens secret. Never log them.
- R5: use one adapter for each transport. Never stack one adapter on another.
- R6: validate server replies tolerantly. Ignore unknown fields.

## Connect an editor over ACP

An editor that speaks the Agent Client Protocol starts `dalgon acp` as a child process. Register `dalgon acp` as the agent command in the editor settings. The command reads one JSON-RPC message per line on standard input. It writes one per line on standard output.

Check the command by hand:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | dalgon acp
```

The reply carries `id` `1` and a `result`.

## Connect a Codex client

`dalgon app-server` serves the Codex app-server protocol. Its frames omit the `jsonrpc` member. A second `initialize` fails with code `-32600`.

```sh
printf '%s\n' '{"id":1,"method":"initialize","params":{"clientInfo":{"name":"gate-client","title":"gate-client","version":"0.1.0"},"capabilities":{}}}' | dalgon app-server
```

The reply carries `id` `1` and a `result`.

## Next

- [Protocol](protocol.md) lists every method, capability, and error code.
- [Embed dal in a Rust program](sdk.md) shows the same agent as a Rust library.
- [Run the dalgon model router and A2A server](serve.md) serves the protocol over WebSockets.
