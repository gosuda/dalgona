# Web UI server

## Scope

`internal/webui` serves the built web UI and relays JSON-RPC between a browser and the daemon.
The caller supplies the listen address, the token, the TLS files, a daemon dialer, and an output writer.

## Assets

The package MUST embed `internal/webui/dist` with `//go:embed all:dist`.
If the root of `dist` holds no entry other than `.gitkeep`, every path except `/rpc` MUST return status 503 with the body `web UI not built; run just web`.
Otherwise the server MUST serve the files in `dist`. `/` MUST serve `index.html`.

## Token

A loopback listen address is `localhost` or a loopback IP. Every other address, including an empty host, is a LAN address.

A LAN address MUST have a configured token of at least 32 characters. Characters are Unicode code points.
Otherwise startup MUST fail before it listens, with this exact error:

```text
web: listening on <addr> needs [web] token (32+ characters)
```

`<addr>` is the configured listen address.

A loopback address MUST use the configured token when one is set.
Otherwise it MUST use a new token for each start. The token MUST be 32 bytes from `crypto/rand`, encoded as unpadded base64url.

A configured token with any other character MUST fail startup with `web: [web] token must contain only [A-Za-z0-9_-]; '+' and '&' cannot survive the URL fragment`.

The server MUST compare SHA-256 digests of the presented token and the expected token with `crypto/subtle.ConstantTimeCompare`.

## TLS

A LAN address MUST use TLS. Without `CertFile` and `KeyFile`, startup MUST fail before it listens with `web: listening on <addr> needs cert_file and key_file for TLS`. `<addr>` is the configured listen address.

When `CertFile` and `KeyFile` are both set, the server MUST serve TLS with `tls.VersionTLS13` as the minimum version.
When only one of them is set, startup MUST fail with `web: cert_file and key_file must both be set`.
A file that does not load MUST fail startup with an error that starts with `web: load cert_file and key_file: `.

## Startup output

All output goes to the caller's writer. Each message is one line.

The server MUST write `http://<addr>/#token=<token>`, or `https://<addr>/#token=<token>` with TLS.
In both lines `<addr>` is the bound listener address. The token stays in the URL fragment, so a browser does not send it in page requests.

If the listener cannot bind, startup MUST fail with `web: listen on <addr>: <error>`.

## RPC relay

`/rpc` MUST require the query parameter `token`. A missing or wrong token MUST get status 401. The server MUST NOT dial the daemon for a rejected request.

For an accepted token, the server MUST dial the daemon with a 10 second limit before the websocket upgrade.
A dial failure MUST get status 502 and MUST write `web: dial daemon: <error>`. A dial failure MUST return the body `daemon unavailable`.
The upgrade MUST use `github.com/coder/websocket` with its default origin check.

The relay MUST NOT decode, re-encode, or change message bytes.

- Browser to daemon: each websocket text message MUST become one line: the message bytes followed by one `\n`, in one write.
- Daemon to browser: each `\n`-terminated line MUST become one websocket text message without the `\n`. The relay MUST stream a line longer than the 64 KiB read buffer as one message.
- A browser message MUST be at most 16 MiB minus 1 byte, which is the daemon line limit without the newline.
- A browser message larger than that limit MUST close the websocket.

Failure behavior:

- A binary message MUST close the websocket with status 1003 (`rpc message is not text`). The daemon MUST receive no bytes from it.
- A text message that contains `\n` MUST close the websocket with status 1007 (`rpc message contains a newline`). The daemon MUST receive no bytes from it.
- When the daemon closes, the server MUST drop an unterminated last line and close the websocket with status 1001.
- When the browser closes, the server MUST close the daemon connection.
- Any other relay failure MUST close the websocket with status 1011 and MUST write `web: rpc relay: <error>`.

## Server limits and shutdown

The HTTP server MUST set a 10 second header read timeout. The HTTP server MUST set a 30 second read timeout. The HTTP server MUST set a 60 second write timeout. The HTTP server MUST set a 120 second idle timeout. A websocket connection is not subject to these limits after the upgrade.

When the context ends, the server MUST stop accepting, end every live relay, close every daemon connection, and only then return.
A clean shutdown MUST return `nil`. Shutdown waits at most 5 seconds for open page requests.

## Browser client

The client in `web/` reads the token from the URL fragment and opens `/rpc`. It speaks the Codex v2 protocol. `web/src/lib/validation.ts` parses each payload the client reads. A parser MUST reject a payload that lacks a member that the schema in `internal/rpc/testdata/codex-schema` or `internal/rpc/testdata/dalgona-schema` requires. `web/src/lib/schema.test.ts` checks this for every Codex payload parser and for the dalgona result and notification parsers.

The page holds one `Session`, which owns the connection and the page state.

### Layout

- Above 52rem the page has two columns, the thread list and the workspace. The workspace holds the status bar, the thread heading, the transcript, and the composer. The transcript scrolls inside its own column.
- At 52rem or narrower the page stacks the parts and the document scrolls. The thread list keeps a maximum height of 18rem.
- At 320px nothing MUST clip or scroll sideways.
- The transcript follows new output only while the reader is within 32px of its end. It follows the transcript at wide widths and the window at narrow widths. Opening a thread follows its end.

### Theme

The `theme` setting takes `auto`, `auto-soft`, `night`, `soft-night`, `day`, or `soft-day`. Any other value MUST read as `auto`.
`auto` MUST follow the system colour scheme and use `night` or `day`. `auto-soft` MUST follow the system colour scheme and use `soft-night` or `soft-day`.
The client MUST cache the setting under the browser key `dalgona.theme` and apply it before the first render.
Each theme MUST use only the colour roles of `DESIGN.md`. The client MUST NOT paint a gradient.
Every text role MUST meet 4.5:1 against its ground. Control edges and the focus ring MUST meet 3:1 against their ground. The ink on the point MUST meet 4.5:1.
Under forced colours, every button edge and focus ring MUST keep a visible border.

### Status bar

The status bar shows the connection state as text. The turn state follows it as text: `Working`, `Turn completed`, `Turn interrupted`, or `Turn failed`.
A `turn/started` notification for a turn already shown as `Turn completed`, `Turn interrupted`, or `Turn failed` MUST NOT replace that status or clear its error.

The client MUST keep the highest applied `seq` for each thread. It MUST ignore a `thread/status/changed` whose `seq` is lower, as `docs/spec/protocol.md` requires.

It also shows model, approval, sandbox, runner, and advisor. The advisor entry comes from `dalgona/advisor/status`. The client loads it when a thread opens or starts, and again after each `dalgona/advisor/note` for the open thread. It shows one of:

- One advisor: its status (`idle`, `reviewing`, `halted`, or `off`) and its note count.
- Several advisors: `<n> advisors · <k> reviewing`.
- `off` when the server answers `-32001`.
- `unavailable` for any other failure.

No advisor failure opens the error banner.

### Transcript

- Item statuses read `Running`, `Done`, `Failed`, `Declined`, or `Waiting for approval`. Other statuses show their raw value.
- Unknown item kinds show their type in plain words, such as `Web search`.
- A `dynamicToolCall` item with tool `advisor` shows as a note card: `advisor · <severity>: <note>`. The note is the text of `contentItems`. The severity is always text. The card's inline-start border marks the severity as a second cue.
- Other dynamic tools show their `contentItems` text as output.
- `turn/plan/updated` shows the latest plan of the current turn as a checklist. Each step has a text status: `Done`, `In progress`, or `Pending`. The step in progress carries `aria-current="step"`. A new turn clears the plan.
- The streaming cursor shows only while the turn runs. The reasoning label reads `Reasoning in progress` while the turn runs and `Reasoning` otherwise.
- A failed turn ends with `The turn failed: <message>. Send your message again to retry, or start a new thread.`
- A turn that is retrying after an error shows `Retrying after an error: <message>`.
- An interrupted turn shows `Turn interrupted. Send a message to continue.`

### Approvals

The client records subagent threads from `dalgona/thread/childSpawned`. An approval from a subagent of the open thread MUST appear in the approval dialog of the open thread. The answer MUST carry the subagent request id. An approval from any other thread MUST count as waiting on another thread. The client MUST keep subagent links per parent thread id when it rebuilds the transcript from `thread/resume` or `thread/read`. It MUST NOT apply links from another thread.

### Empty states and errors

- With no open thread the transcript says `Pick a thread or start a new one`. When connected, it says that earlier threads are in the list. When not connected, it says to connect to dalgona.
- With no connection the thread list says `Threads unavailable` and does not claim that no threads exist. A thread without a title shows its preview. A thread with neither shows `Untitled thread`.
- Selecting Reconnect loads the thread list, then resumes the open thread on the new connection, so its transcript keeps streaming. The resume MUST keep queued follow-ups. Opening a different thread MUST discard them.
- In the approval dialog, Decline and Cancel use the quiet style. The composer's `Interrupt turn` button has the danger style.

### Notices

Two regions MUST be on the page before any notice arrives.
The alert region holds errors. The status region holds warnings and information.
A notice MUST name only the controls that it shows.
Dismiss MUST remove its notice and move focus to the message field.
A notice with the same text as a notice on the page MUST replace it.
The page MUST keep at most five notices. It MUST remove the oldest first.

### Focus

A control that disappears while focused hands focus to the message field. The submit control keeps its element and changes only its label.
