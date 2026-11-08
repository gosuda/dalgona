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

## Methods

### `initialize`

Params contain `clientName`. The result contains `environmentInfo.executorVersion`.

`NewRemote` MUST compare this version with the local binary version. A mismatch MUST fail with:

```
runner: box <id> runs dalgona <v1>; this is <v2>; use the matching box image
```

An unstamped binary MUST use `(devel)` as its version.

### Process methods

`process/start` params contain `processId`, `argv`, `cwd`, `env`, and `sandbox`. `env` is an object. `sandbox` contains `mode` and `writableRoots`. The server MUST pass these values to `Local.Start`. `Remote.Start` MUST send sandbox mode `off`, because the box is the sandbox. A repeated `processId` MUST NOT start a second process.

`process/read` params contain `processId` and `offset`. The result contains `dataBase64`, `next`, `exit`, `alive`, and `path`. A non-null `exit` contains `code`, `signal`, and `endedAtMs`.

`process/signal` params contain `processId` and `signal`. The signal MUST be `term` or `kill`.

`process/terminate` params contain `processId`. It MUST send `kill`. The result MUST contain `running`.

### Filesystem methods

- `fs/readFile` takes `path` and returns `dataBase64`.
- `fs/writeFile` takes `path`, `dataBase64`, and numeric `mode`. A mode outside 0 to 4294967295 MUST produce `-32602`. When `DALGONA_SYNCFS` names a directory, the server MUST call `syncfs` on that directory after the rename completes. A sync failure MUST fail the call.
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

The upload MUST use standard base64 with 76-column lines, each ending in a newline. It MUST run `mkdir -p "$(dirname "$1")" && head -c "$3" | base64 -d > "$1" && chmod "$2" "$1"` with the destination, `0755`, and the exact encoded byte count. The box MUST stop reading after that count, so the upload MUST NOT depend on stdin EOF. A stream with `CloseWrite` MUST still half-close after the last byte. A stream without it MUST stay open until the command exits.

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
