# Two products

## Invariant

`dal` MUST remain the minimal agent and terminal interface. `dalgona` MUST compose the same core with daemon and battery packages. Both products MUST live in one Go module. The repository MUST NOT maintain a private implementation split.

Each product MUST use its own home. `dal` MUST use `$DAL_HOME` or `~/.dal`. `dalgona` MUST use `$DALGONA_HOME` or `~/.dalgona`. Config MUST reject a dalgona-only setting in dal.

The `eval` tool and the tool modes MUST belong to both products. `dal` MUST reach the Starlark engine only through `internal/dal/scripthost`. Plugins, workflows, hooks, MCP servers, skills discovery, remote runners, and the web client MUST stay with `dalgona`.

The shared interpreter seam is the exported `Runtime` in `internal/dal/script` with `NewRuntime`, `LoadCell`, `Call`, `ExecCell`, `WithContext`, `ContextValue`, and `Finish`. The battery-only adapters are `internal/dalgona/script` for plugin and workflow execution and `internal/dalgona/scripthost` for the agent extension. The shared host exposes only `eval` and the neutral extension result.

A relative `$DAL_HOME` or `$DALGONA_HOME` is made absolute by `config.AnchorHome`, and `Load` rejects a relative home. If the user home directory is unknown, the home is the relative `.dal` or `.dalgona` directory.

## Scope

This decision applies to `cmd/dal`, `cmd/dalgona`, and `internal/dal/config`. [README.md](../../README.md) defines the product names. [config.md](../spec/config.md) defines the config boundary.

## Enforcement

The `product-boundary` depguard rule in [.golangci.yml](../../../.golangci.yml) matches `internal/dal` and `cmd/dal` and rejects the `internal/dalgona` prefix, so no battery package links into dal. The `dal` rule keeps the exact `internal/dal/script` engine import out of `cmd/dal`. `cmd/dal` reaches the eval tool only through `internal/dal/scripthost`. The `core` rule keeps interface and battery packages out of the core. The `clients` rule keeps core packages out of the clients. The `tui-imports-only-rpc` rule holds the terminal to `internal/dal/rpc`. The MCP transport test lives in `internal/dalgona/mcp`, not in `internal/dal/app`. Config tests in `internal/dal/config` check product-specific key rejection. `just lint` and `just test` run these checks. `go list -deps ./cmd/dal` MUST show no `internal/dalgona` package and MUST show `internal/dal/scripthost`. `go list -deps ./cmd/dalgona` MUST show at least one battery package.
