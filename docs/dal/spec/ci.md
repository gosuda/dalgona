# Continuous integration

## Triggers

The CI workflow MUST run for pull requests, pushes to `main`, tag pushes, and merge queue groups.
It MUST support manual runs.
Feature branch pushes MUST use their pull request run instead of starting a second CI run.
CI MUST NOT filter changed paths because checks read files across package boundaries.
A newer run for the same ref MUST cancel the older run.
The current workflow has no tag trigger and no concurrency group yet.

## Provider

CI MUST run as one GitHub Actions workflow, `.github/workflows/ci.yml`.
A second CI provider MUST NOT run the same jobs, because the web and Cloudflare tests run once.
The lint, test, vuln, and web CI jobs MUST run on Depot `depot-ubuntu-24.04` runners.
Latchkey MUST NOT run CI.
A job MUST NOT restore a build cache written by another branch. Depot Cache shares entries across the branches of the base repository without isolation.

## Verification

`just verify` MUST run `verify-go` and `verify-shared`.
The CI jobs MUST together run every check from the full verification command.

| CI job | Checks |
|---|---|
| `lint` | `fmt-check`, `vet`, `lint` |
| `test` | `test` |
| `vuln` | `vuln` |
| `web` | `hashline-model`, `spec-style`, `docs-links`, `web-check`, `cf-check` |

The native macOS and Windows lanes are not implemented yet. The current workflow runs the Linux Depot jobs only. Restoring them MUST keep CI in the one workflow.

Go tests MUST keep the race detector and `-count=1`.
The `test` job MUST install `uv` with Python 3.15 for the Go tests that compare against CPython.
A failing job MUST NOT cancel the other jobs.

The `web` job MUST install frozen web dependencies and build the web assets before its web checks.
It MUST activate the `packageManager`-pinned pnpm through corepack before
any pnpm call, because the runner image ships Node without pnpm.

The `web` job MUST install `uv` so the model and specification checks run with the Python 3.15 that `just verify` uses.
The current workflow installs `uv` in no job yet. It runs those checks with the runner's `python3`.
Web tests use Node and happy-dom. Cloudflare tests use local runtime stubs.
Those tests MUST run once on Linux. They do not certify a live browser or Cloudflare deployment.

## Failure reporting

A step in the `web` job MUST run when the steps it needs succeeded, even if an unrelated step failed.
The docs and specs, web, and worker steps are unrelated to each other.
A step MUST be skipped only when a step it needs failed or the run was cancelled.
Cancellation MUST still stop the job.
One run then lists the web and worker test failures together.

## Box image

`just dist` MUST build both commands for every release target.
`just box-dist` MUST build only `dalgona` for Linux amd64 and arm64, with CGO disabled.
Both recipes MUST copy `THIRD_PARTY_NOTICES.md` beside their binaries.
The box image workflow and `just box-image` MUST use `box-dist`.
The image MUST retain both Linux architectures.
The box image publish job MUST use the GitHub Actions runner `depot-ubuntu-24.04`.
