# Agent instructions for [dal and dalgona](README.md)

- You MUST run `just verify` from [justfile](justfile) before every commit.
- You MUST keep the package boundaries in [.golangci.yml](.golangci.yml). A new import across a boundary MUST have a decision in [docs/decisions](docs/README.md#decisions).
- Each behavior change MUST update its topic in [docs/spec](docs/README.md#specifications) in the same commit.
- You MUST keep the journal append-only under [journal.md](docs/spec/journal.md). Recovery MUST follow [recovery.md](docs/spec/recovery.md).
- Hashline profile changes MUST follow [the shared contract](docs/spec/hashline/hashline-common-v1.md), [Light](docs/spec/hashline/hashline-light-v1.md), [Enhanced](docs/spec/hashline/hashline-enhanced-v1.md), and [conformance](docs/spec/hashline/hashline-conformance-v1.md).
- You MUST keep one concern per commit for the module in [go.mod](go.mod). You MUST use the global git identity. You MUST NOT add agent trailers.
- You MUST apply the contribution contract in [CONTRIBUTE.md](CONTRIBUTE.md) and the review contract in [REVIEW.md](REVIEW.md).
