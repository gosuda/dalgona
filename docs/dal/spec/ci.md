# Continuous integration

## Triggers

The CI workflow MUST run for pull requests, tag pushes, and merge queue groups.
Main and feature branch pushes MUST use their pull request or merge queue verification instead of starting a second CI run.
It MUST support manual runs.
CI MUST NOT filter changed paths because checks read files across package boundaries.
A newer pull request run for the same ref MUST cancel the older run.
Tag, merge queue, manual, and scheduled runs MUST NOT cancel one another.
The workflow MUST also run its scheduled verification each Monday at 03:00 UTC.
Pull request runs MUST use a concurrency group per ref.
Other events MUST use a group per run id so a pending run is not replaced.

## Provider

CI MUST run as one GitHub Actions workflow, `.github/workflows/ci.yml`.
A second CI provider MUST NOT run the same jobs, because the web and Cloudflare tests run once.
Every CI job MUST run on a Depot runner.
Linux jobs MUST use `depot-ubuntu-24.04`, macOS jobs MUST use `depot-macos-latest`, and Windows jobs MUST use `depot-windows-2025-8`.
Latchkey MUST NOT run CI.
A job MUST NOT restore a build cache written by another branch. Depot Cache shares entries across the branches of the base repository without isolation.
Go setup MUST disable its shared cache. Explicit cache keys and restore prefixes MUST include the current ref, runner operating system, architecture, and Go version.
They MUST also include the job and matrix lane so concurrent checks can save their own build and lint outputs.
Depot runners force a non-empty `GOCACHEPROG` into every step process.
An empty environment value does not clear it.
Every step that invokes the go command MUST unset `GOCACHEPROG` in its shell first.
PowerShell steps MUST remove it from `Env:`.
The pinned Go setup action MUST preload `go-cache-paths/isolate.cjs` before invoking Go.
The preload MUST remove the runner's cache program and select an isolated `GOENV` file containing an explicit empty `GOCACHEPROG` entry.
It MUST export that file for later steps, so neither user settings nor toolchain defaults select a remote backend.
The shared `go-cache-paths` action MUST unset the variable and verify the isolated file is active and effective `go env GOCACHEPROG` is empty before restoring caches.
It MUST log the effective Go cache paths and report a named error when a configuration check fails.
Neither a persisted Go setting nor Depot's automatic remote Go cache bypasses the explicit ref partition.
It MUST discover the module and build cache paths through `go env`.
The explicit cache action MUST restore those resolved paths.
It MUST set the linter cache path explicitly under the runner temporary directory.
A new merge-group ref MUST start with an isolated cache instead of restoring another ref's compiler output.

## Verification

`just verify` MUST run `verify-go` and `verify-shared`.
The CI jobs MUST together run every check from the full verification command.

| CI job | Checks |
|---|---|
| `lint` | `fmt-check`, `vet`, `lint` |
| `test` | `test` |
| `vuln` | `vuln` |
| `web` | `hashline-model`, `spec-style`, `docs-links`, `web-check`, `cf-check` |
| `Verify (macos-latest)` | `vet`, `verify-native-build`, `verify-native-test` on scheduled, manual, and tag runs |
| `Verify (windows-latest)` | `vet`, `verify-native-build`, `verify-native-test` on scheduled, manual, and tag runs |

The Go matrix MUST preserve these check names and MUST disable fail-fast.
Linux Go lanes MUST install the graphics development packages needed to build the native client.

The Linux `test` job MUST keep the race detector and `-count=1`.
Native tests MUST keep `-count=1`.
The Linux lane supplies race detection.
The `test` job MUST install `uv` with Python 3.15 for the Go tests that compare against CPython.
A failing job MUST NOT cancel the other jobs.

The `web` job MUST install frozen web dependencies and build the web assets before its web checks.
It MUST activate the `packageManager`-pinned pnpm through corepack before
any pnpm call, because the runner image ships Node without pnpm.

The `web` job MUST install `uv` so the model and specification checks run with the Python 3.15 that `just verify` uses.
Every Python check in the workflow MUST use `uv run --no-project --python 3.15`.
Web tests use Node and happy-dom. Cloudflare tests use local runtime stubs.
Those tests MUST run once on Linux. They do not certify a live browser or Cloudflare deployment.

## Failure reporting

A step in the `web` job MUST run when the steps it needs succeeded, even if an unrelated step failed.
Each check command in the `web` job MUST be its own step, because a failing command ends the rest of its step.
The docs and specs checks, the web checks, and the worker checks are unrelated to each other.
A step MUST be skipped only when a step it needs failed or the run was cancelled.
Cancellation MUST still stop the job.
One `web` job run then lists every failing check that does not depend on a failed step.

## Native lanes

macOS and Windows Go and desktop jobs MUST run only on scheduled, manual, and tag runs.
Routine pull requests and merge queue groups MUST run the Linux desktop job and all four Linux verification lanes.
The weekly native runs supply operating system checks that routine pull request and merge-queue runs do not execute.
Windows desktop static checks, GUI tests, packaging validation, and the binary build MUST share one job and one dependency setup.
The `web` job uploads its `dist` artifact for the native jobs to download before Go verification.
Each native Go host MUST run vet, build, and tests.
The Linux lint and vulnerability jobs supply shared static and advisory checks.
Each native desktop host MUST retain its GUI-specific checks and packaging validation.
The `desktop-check` recipe MUST compose `desktop-check-static` and `desktop-check-gui` without copying their commands.
A failing native job MUST NOT cancel the other native jobs.

The `web` job MUST upload `internal/dalgona/webui/dist` only after all its checks pass.
The artifact MUST include hidden files and MUST come from the same workflow run as its consumers.
An upload with no files MUST fail.
Every Go lane MUST download the artifact before Go verification so compilation embeds the same built assets.
An artifact digest mismatch MUST fail the download.
The artifact MUST expire after seven days.
Rerunning the `web` job MUST replace its previous artifact.
After expiry, a rerun MUST include the `web` job to rebuild the artifact.

The native jobs MUST use the check names `Verify (macos-latest)` and `Verify (windows-latest)`.
If the `web` job fails or skips, each native check MUST fail before tool setup.
Workflow cancellation MUST still cancel the native jobs.

## Box image

`just dist` MUST build both commands for every release target.
`just box-dist` MUST build only `dalgona` for Linux amd64 and arm64, with CGO disabled.
Both recipes MUST copy `THIRD_PARTY_NOTICES.md` beside their binaries.
The box image workflow and `just box-image` MUST use `box-dist`.
The image workflow MUST use the same isolated Go setup preload and cache-path guard as CI.
It MUST disable setup-go caching and unset `GOCACHEPROG` before compiling box binaries.
The image MUST retain both Linux architectures.
The box image publish job MUST use the GitHub Actions runner `depot-ubuntu-24.04`.
