# Continuous integration

## Triggers

The CI workflow MUST run for pull requests, pushes to `main`, tag pushes, and merge queue groups.
It MUST support manual runs.
Feature branch pushes MUST use their pull request run instead of starting a second CI run.
CI MUST NOT filter changed paths because checks read files across package boundaries.
A newer run for the same ref MUST cancel the older run.

## Provider

The lint, test, vuln, and web CI jobs MUST run on Depot `depot-ubuntu-24.04` runners.
Latchkey MUST NOT run CI.
A Go job MUST NOT restore a build cache written by another branch. Depot Cache shares entries across the branches of the base repository without isolation.

## Verification

`just verify` MUST run `verify-go` and `verify-shared`.
The groups MUST retain every check from the full verification command.

| Group | Checks | CI hosts |
|---|---|---|
| `verify-go` | `fmt-check`, `vet`, `lint`, `test`, `vuln` | Linux, macOS, and Windows |
| `verify-shared` | `hashline-model`, `spec-style`, `docs-links`, `web-check`, `cf-check` | Linux |

The native macOS and Windows lanes are not implemented yet. The current workflow runs the Linux Depot lanes only. Restoring them MUST keep the Provider rule's job scope.

Go tests MUST keep the race detector and `-count=1`.
Each native host MUST run its own Go lint, vet, tests, and vulnerability checks.
Each native host MUST install `uv` with Python 3.15 for the Go tests that compare against CPython.
A failing native job MUST NOT cancel the other native jobs.

The shared job MUST install frozen web dependencies and build the web assets before its checks.
It MUST cache the pnpm store with both pnpm lockfiles in the cache key.
It MUST install `uv` so the model and specification checks run in CI.
Web tests use Node and happy-dom. Cloudflare tests use local runtime stubs.
Those tests MUST run once on Linux. They do not certify a live browser or Cloudflare deployment.

## Artifact handoff

The shared job MUST upload `internal/dalgona/webui/dist` only after all shared checks pass.
The artifact MUST include hidden files and MUST come from the same workflow run as its consumers.
An upload with no files MUST fail.
Each native job MUST download the artifact before Go verification so compilation embeds the same built assets.
The artifact MUST expire after seven days.
Rerunning the shared job MUST replace its previous artifact.
After expiry, a rerun MUST include the shared job to rebuild the artifact.

The native jobs MUST retain the `Verify (ubuntu-latest)`, `Verify (macos-latest)`, `Verify (windows-latest)` check names.
If the shared job fails or skips, each native check MUST fail before tool setup.
Workflow cancellation MUST still cancel the native jobs.

## Box image

`just dist` MUST build both commands for every release target.
`just box-dist` MUST build only `dalgona` for Linux amd64 and arm64, with CGO disabled.
Both recipes MUST copy `THIRD_PARTY_NOTICES.md` beside their binaries.
The box image workflow and `just box-image` MUST use `box-dist`.
The image MUST retain both Linux architectures.
The box image publish job MUST use the GitHub Actions runner `depot-ubuntu-24.04`.
