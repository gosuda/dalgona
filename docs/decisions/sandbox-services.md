# Sandbox services

## Invariant

Remote execution MUST use the `Service` seam in `internal/sandboxsvc`. A service MUST create, attach, connect, and destroy a box. Callers MUST use the identifier returned by attach, because a service MAY replace the box.

[runners.md](../spec/runners.md) lists the supported services. Exec and subagents MAY run in any listed box while the orchestrator stays local. A whole-daemon box MUST use a service that [runners.md](../spec/runners.md) allows for fully cloud mode.

Modal replacement MUST retain the named volume. Sandbox0 MUST use a template, not an image. [sandbox-services.md](../spec/sandbox-services.md) defines these persistence contracts. [exec-server.md](../spec/exec-server.md) defines the common wire, bootstrap, and binary-version checks.

## Scope

This decision applies to `internal/sandboxsvc`, remote runners, and cloud-daemon setup. The service seam hides provider transport. It MUST NOT create a second tool or agent protocol.

The approved service selection excludes Daytona because the selection record marked it archived on 2026-11-08. It excludes E2B, Vercel, and Runloop because that record found no Go SDK. It excludes Namespace and Coder because their Go SDK required the full monorepo. These are selection reasons from that date, not claims about current vendor maintenance.

## Enforcement

`internal/sandboxsvc/modal/service_test.go` checks replacement on the same volume and cleanup after bootstrap failure. `internal/sandboxsvc/sandbox0/service_test.go` checks workspace setup failure and deletion. `internal/config/config_test.go` rejects a Sandbox0 image. Service tests in `internal/sandboxsvc` cover the adapters. `just test` runs them. A live test that skips without credentials MUST NOT count as a live-service pass. Whole-daemon Cloudflare refusal MUST have a command-level test before that mode ships.
