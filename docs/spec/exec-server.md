# Exec server

## Purpose

The exec server runs processes and filesystem operations through one `Local` runner. A `Remote` runner calls it over a bidirectional stream. The same wire protocol works over standard input and output, an in-process stream, or a websocket.

## Framing

- Messages MUST use JSON-RPC 2.0. Each message MUST occupy one line. The maximum line size is 16 MiB.
- Requests MUST contain `id`, `method`, and `params`. The server MUST ignore unknown parameter fields.
- File and process output bytes MUST use standard base64 in `dataBase64`.
- Malformed JSON MUST produce error `-32700`. An unknown method MUST produce `-32601`. Invalid parameters MUST produce `-32602`.
- A missing process MUST produce `-32001`. A missing file MUST produce `-32002`. Other operation failures MUST produce `-32000`.
- The server MUST close the stream when cancellation ends the read operation. The client MUST wake every pending call when the stream closes.
- When `TERM` fails because the stream closed and the runner can redial, the caller MUST open one fresh connection within 2 s and MUST send `TERM`, poll, and `KILL` over it.

## Methods

### `initialize`

Params contain `clientName`. The result contains `environmentInfo.executorVersion`.

`NewRemote` MUST compare this version with the local binary version. A mismatch MUST fail with:

```
runner: box <id> runs dalgona <v1>; this is <v2>; use the matching box image
```

An unstamped binary MUST use `(devel)` as its version.

### Process methods

`process/start` params contain `processId`, `argv`, `cwd`, `env`, and `sandbox`. `env` is an object. `sandbox` contains `mode` and `writableRoots`. The server MUST pass these values to `Local.Start`. `Remote.Start` MUST send sandbox mode `off`, because the box is the sandbox. A repeated `processId` MUST NOT start a second process. A request arriving over the websocket box channel MUST NOT weaken the sandbox below the server start mode: when the server floor is `workspace-write`, `process/start` with any other mode MUST fail with `-32602`. The floor is `workspace-write` only when the server itself started with `DALGONA_SANDBOX_MODE=workspace-write`; otherwise the box is the sandbox and `off` stays allowed.

`process/read` params contain `processId`, `offset`, and `limit`. The result contains `dataBase64`, `next`, `exit`, `alive`, and `path`. A non-null `exit` contains `code`, `signal`, and `endedAtMs`. The server MUST return at most `limit` bytes, defaulting to 1 MiB for a missing or out-of-range `limit`, so no response line exceeds the 16 MiB framing cap. The client MUST send `limit` and MUST keep reading from `next` while a page is full.

`process/signal` params contain `processId` and `signal`. The signal MUST be `term` or `kill`.

`process/terminate` params contain `processId`. It MUST send `kill`. The result MUST contain `running`.

### Filesystem methods

- `fs/readFile` takes `path`, `offset`, and `limit`, and returns `dataBase64` with the `next` offset. Pages MUST be at most `limit` bytes, defaulting to 1 MiB, and the client MUST loop from `next` until a short page arrives.
- `fs/writeFile` takes `path`, `dataBase64`, numeric `mode`, and `offset`. Offset zero MUST replace the file atomically. A nonzero `offset` MUST write one chunk of a paged upload at that byte position; the client MUST split large files into 1 MiB chunks so no request line exceeds the 16 MiB framing cap. The client MUST stage a multi-chunk upload in a temporary file in the destination directory and MUST publish it with `fs/rename` only after the last chunk succeeds. The client MUST derive the staging directory from the destination text. When the destination contains `/`, the directory ends after the last `/` and `\` is a name byte; otherwise, when it contains `\`, the directory ends after the last `\`; otherwise the staging name stands alone. An empty destination MUST fail before any upload. The staging basename MUST be `.dalgona-partial-` followed by 16 lowercase hex digits from a cryptographic random source, 33 bytes in total, so the destination name length cannot break the staging name. Each upload selects 64 random bits, so a collision between concurrent uploads is improbable. The first chunk MUST replace an existing staging name atomically, so a collision loses the older upload's staged data. The basename MUST NOT reuse the destination name, so it cannot shadow a user file, and the `.dalgona-partial-` prefix MUST let a later cleanup recognize a file a crashed upload left behind. The client MUST remove the staged file when a chunk or the rename fails. The removal MUST run under a context detached from the upload context with a 5 s timeout, so a silent server cannot block the caller past the timeout. The client MUST skip the removal when no chunk call returned. A mode outside 0 to 4294967295 MUST produce `-32602`. A negative `offset` MUST produce `-32602`. On Linux, when `DALGONA_SYNCFS` names a directory, the server MUST call `syncfs` on that directory after the rename completes. A sync failure MUST fail the call. On other systems the variable MUST have no effect.
- `fs/rename` takes `from` and `to`. The server MUST publish `from` at `to` with one atomic rename. The server MUST sync the parent directory after the rename completes. On Windows the parent directory sync MUST be a no-op. On Linux, when `DALGONA_SYNCFS` names a directory, the server MUST call `syncfs` on that directory after the rename completes. A sync failure MUST fail the call. On other systems the variable MUST have no effect.
- `fs/getMetadata` takes `path` and returns `name`, `size`, `mode`, `modTimeMs`, `isDirectory`, and `isFile`.
- `fs/readDirectory` takes `path` and returns `entries`. Each entry uses the metadata fields above.
- `fs/remove` takes `path`.

The server MUST preserve the runner operation semantics. The client MUST preserve `ErrProcessNotFound` and `fs.ErrNotExist` through `errors.Is`.

## Server command

`ExecServerMain` accepts `--listen stdio://` or `--listen ws://IP:PORT`, plus `--root DIR` and optional `--data DIR`.

Websocket mode MUST require `DALGONA_BOX_TOKEN` with at least 32 characters. Startup without it MUST fail with:

```
exec-server: ws listen needs DALGONA_BOX_TOKEN (32+ characters)
```

Every websocket request MUST carry `Authorization: Bearer <token>`. The server MUST compare tokens in constant time. A wrong token MUST receive HTTP 401. Other paths MUST receive HTTP 404.

Path `/` serves the exec protocol. Path `/bridge` calls the supplied `BridgeFunc`. A missing bridge MUST produce HTTP 503.

With `--data`, the server MUST create `<data>/workspace` and `<data>/home`. It MUST link the selected root to the workspace and set `HOME` to the home directory. An existing non-symlink root MUST NOT be removed.

## Bootstrap

`Bootstrap` MUST probe `uname -m` and `~/.dalgona/bin/dalgona --version`. It MUST support `amd64` and `arm64`. The remote version is the last whitespace-separated field of the `--version` output. A version equal to the local binary version MUST skip the upload. A version mismatch MUST select the matching local Linux binary and upload it to `~/.dalgona/bin/dalgona` with mode 0755.

The upload MUST use standard base64 with 76-column lines, each ending in a newline. It MUST run `mkdir -p "$(dirname "$1")" && tmp="$1.tmp.$$" && trap 'rm -f "$tmp"' EXIT && head -c "$3" | base64 -d > "$tmp" && chmod "$2" "$tmp" && mv "$tmp" "$1"` with the destination, `0755`, and the exact encoded byte count. The decode MUST land in a temporary file in the same directory, and the install MUST be an atomic rename, so a running detached binary never sees a truncated page and the write never hits `ETXTBSY`. The box MUST stop reading after that count, so the upload MUST NOT depend on stdin EOF. A stream with `CloseWrite` MUST still half-close after the last byte. A stream without it MUST stay open until the command exits.

A missing sibling binary MUST report:

```
runner: no linux/<arch> dalgona binary next to <exe>; build it with GOOS=linux GOARCH=<arch> CGO_ENABLED=0 go build -o <path> ./cmd/dalgona
```

A box without `base64` MUST report:

```
runner: box <id> has no base64; use an image with coreutils
```

## Workspace upload

`Upload` MUST enumerate `git ls-files -co --exclude-standard -z` in a git work tree. Outside a work tree it MUST walk regular files and skip `.git`. Both paths MUST skip `.dalgona-baseline`.

- At most 16 file writes MAY be in flight.
- Remote paths MUST be `/workspace/<slash-relative-path>`.
- Files MUST retain their permission bits. Files with mode 0755 MUST also pass through the prescribed `chmod +x` batch.
- The server MUST run `git init -q && git add -A && git write-tree` in `/workspace` and require exit code zero.
- The returned tree ID MUST be written to `/workspace/.dalgona-baseline`.
- Process output MUST be read until the durable exit record appears. A process without an exit record MUST NOT be treated as complete.
