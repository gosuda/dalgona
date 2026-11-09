# Two products

## Invariant

`dal` MUST remain the minimal agent and terminal interface. `dalgona` MUST compose the same core with daemon and battery packages. Both products MUST live in one Go module. The repository MUST NOT maintain a private implementation split.

Each product MUST use its own home. `dal` MUST use `$DAL_HOME` or `~/.dal`. `dalgona` MUST use `$DALGONA_HOME` or `~/.dalgona`. Config MUST reject a dalgona-only setting in dal.

## Scope

This decision applies to `cmd/dal`, `cmd/dalgona`, and `internal/config`. [README.md](../../README.md) defines the product names. [config.md](../spec/config.md) defines the config boundary.

## Enforcement

The `dal` depguard rule in [.golangci.yml](../../.golangci.yml) rejects battery imports from `cmd/dal`. The `core` and `clients` rules keep interface packages out of the core and core packages out of clients. Config tests in `internal/config` check product-specific key rejection. `just lint` and `just test` run these checks.
