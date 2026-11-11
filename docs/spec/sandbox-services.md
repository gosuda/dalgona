# Sandbox services

The daemon MUST expose one service seam for remote boxes. A service MUST create, attach, connect, and destroy a box. A pipe service MUST run commands in `/workspace`. The exec-server endpoint MUST run `$HOME/.dalgona/bin/dalgona exec-server --listen stdio:// --root /workspace`. The bridge endpoint MUST run `$HOME/.dalgona/bin/dalgona bridge`. Bootstrap failures MUST fail attach.

## Modal

`NewFromEnv` MUST use the Modal default profile. The profile MUST come from the Modal environment or `~/.modal.toml`.

Create MUST use the app `dalgona`. It MUST create the app when absent. The default image MUST be `docker.io/library/buildpack-deps:stable`. A configured image MUST replace the default.

Create MUST use a named volume with version 2. The volume name MUST equal the box name. Create MUST mount the volume at `/data`. The sandbox name MUST have the form `<box>-<n>`, where `n` starts at 1. Name collisions MUST advance `n`. Create MUST stop after 100 collisions.

The sandbox MUST have a 24-hour timeout and a one-hour idle timeout. It MUST set `HOME=/data/home` and `DALGONA_SYNCFS=/data`. Its entry command MUST create `/data/workspace` and `/data/home`. It MUST link `/workspace` to `/data/workspace` and then sleep indefinitely.

A box identifier MUST have the form `<sandbox-id>/<volume-name>`.

Attach MUST poll the sandbox. A running sandbox MUST retain its identifier. A finished sandbox MUST cause the service to create the next numbered sandbox on the same volume. Attach MUST return the new identifier. It MUST run bootstrap against the replacement. A bootstrap failure MUST terminate the replacement without deleting the volume. The volume MUST retain `/workspace` and `/data/home` across this replacement.

Destroy MUST terminate the sandbox and wait for termination. It MUST then delete the named volume. A termination failure MUST prevent volume deletion.

A process MUST pipe stdin, stdout, and stderr. Reads MUST serve stdout. Process closure MUST close all three streams. A process wait MUST end no later than 25 hours after the wait starts.

## Sandbox0

`NewFromEnv` MUST require `SANDBOX0_TOKEN`. `SANDBOX0_BASE_URL` MUST override the default service URL when set.

Create MUST claim the configured template. An empty template MUST mean `default`. The claim MUST set a 1800-second soft lifetime, a zero hard lifetime, and automatic resume to false.

Create MUST run this workspace command through a CMD context:

```sh
mkdir -p /workspace 2>/dev/null || { sudo -n mkdir -p /workspace && sudo -n chown "$(id -u)" /workspace; }
```

A command error, missing exit code, or nonzero exit code MUST fail create. Create MUST delete the claimed sandbox before returning this error:

```text
runner: sandbox0 template <template> cannot create /workspace; use a template whose user can write it
```

Attach MUST return the same identifier. It MUST resume a paused sandbox and wait for the resume. A bootstrap failure MUST fail attach.

The writable root filesystem MUST persist across pause and resume. `/tmp` has no persistence guarantee. Paused processes MUST NOT be reported as live after resume.

Each open command stream MUST start a refresh task. The task MUST refresh the sandbox every 10 minutes. Stream closure or context cancellation MUST stop the task. Refresh failures MUST NOT close the stream.

The stream adapter MUST send each write as one `SendInput` call with an empty request identifier. Reads MUST return only stdout data. A read smaller than the available stdout chunk MUST retain the remainder for the next read. Reads MUST log stderr at debug level without returning it. A done message MUST return `io.EOF`. A zero-length read MUST NOT receive from the remote stream.

Close MUST send `TERM` before closing the remote stream. It MUST attempt the remote close after a signal failure. Repeated close calls MUST have no additional remote effect.

## Cloudflare

Every box name MUST match `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`, the worker route rule. Create, Attach, Destroy, and Connect MUST fail before any request with `cloudflare: invalid box name` when it does not. The worker MUST check authorization first and answer 401 when it fails. For an authorized request whose box name breaks the rule, the worker MUST answer 400 `invalid box name`. It MUST answer 404 for unknown routes.

The worker MUST accept only `DALGONA_TOKEN` on worker routes. It MUST compare the bearer in constant time. A box token MUST NOT pass worker authorization. The client MUST send `DALGONA_TOKEN` to the worker.

The durable object MUST read the box name from `ctx.id.name`. A missing box name MUST fail with 500. It MUST derive the box token with HMAC-SHA256 over the box name keyed by `DALGONA_TOKEN`. It MUST set `DALGONA_BOX_TOKEN` to the box token. It MUST replace the Authorization header with the box token when it proxies into the container.
