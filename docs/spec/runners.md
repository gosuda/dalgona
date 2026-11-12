# Runners and remote modes

Package `internal/remote` binds threads to boxes. Mode A runs exec and subagents in a box while the orchestrator stays local. Mode B runs the whole daemon in a box. The service seam, bootstrap, and workspace upload are in `sandbox-services.md` and `exec-server.md`.

## Services

A `[runners.<name>]` entry MUST name one service: `docker`, `sprites`, `modal`, `sandbox0`, or `cloudflare`. The binder MUST build one service per distinct runner entry and reuse it.

| Service | Constructor | Credential |
|---|---|---|
| `docker` | `docker.NewFromEnv` | Docker environment |
| `sprites` | `sprites.NewFromToken` | env `SPRITES_TOKEN`, else `auth.json` key `sprites` |
| `modal` | `modal.NewFromEnvWithImage` when `image` is set, else `modal.NewFromEnv` | Modal environment or `~/.modal.toml` |
| `sandbox0` | `sandbox0.New` | env `SANDBOX0_TOKEN`, else `auth.json` key `sandbox0`; env `SANDBOX0_BASE_URL` overrides the URL |
| `cloudflare` | `cloudflare.New(url, token)` | env `DALGONA_CLOUDFLARE_TOKEN`, else `auth.json` key `cloudflare` |

Every service except `cloudflare` MUST receive `runner.Bootstrap`. A credential MUST come from its environment variable, else from the `auth.json` entry for the service. The binder MUST NOT read the generic `<SERVICE>_API_KEY` variable. A missing credential MUST fail with ``remote: <service> needs <ENV> or `dalgona login <service> --api-key` ``. A `cloudflare` entry without `url` MUST fail.

## Mode A: thread boxes

`(*Binder).Runner` MUST match `agent.RunnerFactory`. A runner name that is not a `[runners.<name>]` entry MUST go to `Options.Local`. With no `Options.Local`, the factory MUST fail.

The box runner MUST bind on first use. Binding MUST read the thread journal for `session.created`, the latest `runner.bound`, and `thread.archived`. It MUST NOT bind before the first runner call, so `session.created` and a fork's copied `runner.bound` are already in the journal. A journal damaged at its tail MUST be read up to the damage. A tagged record that does not decode MUST fail the bind.

A fork without `runner.bound` MUST use the latest `runner.bound` of its `fork_of` thread when that thread has one, and record it.

Otherwise the thread MUST create one box named `dalgona-<thread_id>`. Create MUST pass the entry's `image` and `template`. When Create fails, the binder MUST attach a box by that name. That attach recovers a box left by a crash before `runner.bound` was recorded. The binder MUST attach the box, connect `ExecServer`, open `runner.Remote`, upload the thread `cwd` with `runner.Upload`, and record `runner.bound` with `runner`, `service`, `box_id`, and `baseline`. A failure after create MUST destroy the box. A fork that creates the box MUST also record it for its `fork_of` thread. When that thread is not open, the binder MUST log this and continue.

A thread with `runner.bound` MUST attach its `box_id` after it follows the replacement entries in the box ledger. It MUST NOT create a box. This covers a daemon restart and a fork. When attach returns another id (Modal replacement), the binder MUST append `replaced <service> <old> <new>` to the ledger. It MUST record `runner.bound` with the new `box_id` and the old `baseline` for this thread. It MUST record the same for every other unarchived thread whose binding resolves to the old box. A thread that is not open MUST be skipped with an info log, and it resolves the ledger on its next bind.

For `cloudflare` only, attach MUST check `/workspace/.dalgona-baseline`. When the file is missing, the binder MUST upload the workspace again. The binder MUST record `runner.bound` with the new baseline. The binder MUST write this line to `Options.Out` and log it at warn level:

```text
box <box_id> was replaced; unpulled box changes are lost
```

The binder MUST record `runner.bound` through `Recorder.RecordRunnerBound(ctx, threadID, bound)`. `*agent.Host` implements it by appending through the thread actor. With no recorder, binding MUST fail.

The `Attach` hook of the daemon host MUST call `SetRecorder` with the new `*agent.Host`, because the host is the recorder.

Paths MUST map from the thread `cwd` to `/workspace`. `cwd` maps to `/workspace`. `cwd/a/b` maps to `/workspace/a/b`. An empty `ProcessSpec.Dir` maps to `/workspace`. Relative paths and paths outside `cwd` MUST pass through unchanged. Box path mapping MUST use POSIX path rules on every host.

Every remote box runs Linux. The exec tool MUST build remote commands with `sh -c` and MUST keep `cmd /C` for local Windows execution only.

Concurrent calls on one runner MUST share one bind. The lock MUST NOT be held during network calls. When a call fails and the exec stream is dead, the runner MUST reconnect by `box_id` and retry that call once. `Start` is idempotent per call id, and `Read` continues by call id, so commands survive stream loss.

`Close` MUST close the exec stream. It MUST NOT destroy the box.

## Archive

`(*Binder).Archive(ctx, threadID)` MUST run as the archive hook, after `Host.Archive` journals `thread.archived`. If the archive hook fails, the server MUST unarchive the thread. A thread without `runner.bound` MUST succeed with no effect. Archived state MUST come from each thread's latest `thread.archived` record.

When another unarchived thread has a latest `runner.bound` that resolves to the same box, Archive MUST keep the box and log at info level:

```text
runner: box <box_id> kept; still used by thread <id>
```

Otherwise Archive MUST destroy the box and append `destroyed <service> <box_id>` to the box ledger.

The box ledger is `<home>/boxes.log`. Each line MUST be `destroyed <service> <box_id>` or `replaced <service> <old> <new>`. Unknown lines MUST be ignored.

## Garbage collection

`dalgona runner gc` MUST list each box whose bound threads are all archived and that the ledger does not mark destroyed. Each line MUST name the service, box id, runner, and archived threads. It MUST then ask `destroy <n> unbound boxes? [y/N]`. Only `y` or `yes` MUST destroy them. With no such box it MUST print `no unbound boxes`.

## Pull

`dalgona pull <thread>` MUST attach the thread's box and run in `/workspace`:

```sh
git add -A && git diff --no-ext-diff --no-color --binary --cached <baseline> -- . ':(exclude).dalgona-baseline'
```

The box writes the diff to `$HOME/.dalgona/pull.diff` and prints that path.

An empty diff MUST print `nothing to pull` and succeed. Otherwise the diff MUST apply in the local `cwd` with `git apply --3way`. When `cwd` is a repository subdirectory, apply MUST pass `--directory=<git rev-parse --show-prefix>`, because box paths are relative to `cwd`. Outside a git work tree it MUST apply with `git apply`. Files left unmerged MUST make the command exit 1 and print their paths relative to `cwd`, read with `git diff --name-only --relative -z --diff-filter=U`:

```text
remote: pull left conflict markers in:
  <file>
```

Files that `git apply --3way` leaves with conflict markers MUST be listed in `ConflictError`.

In a git work tree, `Binder.Pull` MUST return a `*ConflictError` that lists the unmerged files.

Pull MUST NOT move the baseline. A second pull MUST diff against the same baseline.

## Mode B: the daemon in a box

`[remotes.<name>]` in `settings.toml` MUST hold `service`, `box_id`, and the optional `image`, `template`, and `url`. The binder MUST write the whole table in one `config.WriteSettings` call. A name without `[remotes.<name>]` MUST fall back to `[runners.<name>]`.

`cloudflare` MUST be refused by `cloud up`, `cloud down`, `cloud pull`, and `--remote` with:

```text
cloudflare boxes lose their files when the container stops; use sandbox0, modal, sprites, or docker for fully cloud mode
```

`dalgona cloud up <name> [--copy-auth]` MUST attach the recorded box, or create `dalgona-remote-<name>` when `box_id` is empty. The new `box_id` MUST be recorded right after Create, before attach or bootstrap. A failed attach then leaves a box that `cloud up` and `cloud down` can reach. It MUST upload the local workspace when the box has no `/workspace/.dalgona-baseline`. With `--copy-auth` it MUST write the local `auth.json` to `$HOME/.dalgona/auth.json` in the box with mode 0600 through the exec server's file write. A copy failure MUST be returned before the success line. Without `--copy-auth`, it MUST print ``sign in inside the box with `dalgona --remote <name> login` ``.

`dalgona cloud down <name> [--yes]` MUST ask `destroy <service> box <box_id> and every thread in it? [y/N]` unless `--yes` is set. On yes it MUST destroy the box, which holds the daemon journal, and set `box_id` to `""`.

`dalgona cloud pull <name>` MUST read the baseline from `/workspace/.dalgona-baseline` and apply the box diff in the local `cwd`, as Pull does.

`(*Binder).Bridge(ctx, name)` MUST attach the recorded box and return `Connect(Bridge)`. When attach returns another id, it MUST write the new `box_id` before it connects. A remote without `box_id` MUST fail with:

```text
remote: remote <name> has no box; run `dalgona cloud up <name>`
```

## Entry points

```go
func NewBinder(opts Options) *Binder
func (b *Binder) SetRecorder(r Recorder)
func (b *Binder) Runner(ctx context.Context, threadID, runnerName string) (runner.Runner, error)
func (b *Binder) Archive(ctx context.Context, threadID string) error
func (b *Binder) Pull(ctx context.Context, threadID string) error
func (b *Binder) GC(ctx context.Context) ([]Box, error)
func (b *Binder) CloudUp(ctx context.Context, name string, copyAuth bool) error
func (b *Binder) CloudDown(ctx context.Context, name string, yes bool) error
func (b *Binder) CloudPull(ctx context.Context, name string) error
func (b *Binder) Bridge(ctx context.Context, name string) (io.ReadWriteCloser, error)

type IO struct { Stdin io.Reader; Stdout, Stderr io.Writer }

func PullMain(ctx context.Context, opts Options, args []string, stdio IO) int
func RunnerMain(ctx context.Context, opts Options, args []string, stdio IO) int
func CloudMain(ctx context.Context, opts Options, args []string, stdio IO) int
```

The `Main` functions MUST return 0 on success, 1 on failure, and 2 on a usage error. They MUST ask questions on `Stdout` and read answers from `Stdin`.

Without `Options.Journal`, `Pull`, `Archive`, `GC`, and runner binding MUST fail. `CloudUp`, `CloudDown`, and `CloudPull` MUST NOT need `Options.Journal`.
