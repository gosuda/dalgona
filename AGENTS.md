# Agent instructions for [dal and dalgona](README.md)

## Core philosophy

Be opinionated. Do not add every piece of junk in the world. Do not ship something naively empty.

- Pick one answer per decision and add no option to avoid the choice. [Steve Jobs]
- Name the user task before adding a feature, command or layer. [Steve Jobs]
- Never remove a control, label, state, check or error path the user needs and call it minimal. [Steve Jobs]
- A feature works from start to finish with its defaults before the next one is added. [Steve Jobs]

## Design

- Each visible element serves the task. [Dieter Rams]
- Leave space empty and never hide what the user needs to act. [Kenya Hara]
- Interface changes follow [DESIGN.md](DESIGN.md). [Dieter Rams, Kenya Hara]

## Contracts

- You MUST run `just verify` from [justfile](justfile) before every commit. [Steve Jobs]
- You MUST keep the package boundaries in [.golangci.yml](.golangci.yml). A new import across a boundary MUST have a decision in [docs/decisions](docs/README.md#decisions). [Steve Jobs]
- Each behavior change MUST update its topic in [docs/spec](docs/README.md#specifications) in the same commit. [Steve Jobs]
- You MUST keep the journal append-only under [journal.md](docs/spec/journal.md). Recovery MUST follow [recovery.md](docs/spec/recovery.md). [Steve Jobs]
- Hashline profile changes MUST follow [the shared contract](docs/spec/hashline/hashline-common-v1.md), [Light](docs/spec/hashline/hashline-light-v1.md), [Enhanced](docs/spec/hashline/hashline-enhanced-v1.md), and [conformance](docs/spec/hashline/hashline-conformance-v1.md). [Steve Jobs]
- You MUST keep one concern per commit for the module in [go.mod](go.mod). You MUST use the global git identity. You MUST NOT add agent trailers. [Steve Jobs]
- You MUST apply the contribution contract in [CONTRIBUTE.md](CONTRIBUTE.md) and the review contract in [REVIEW.md](REVIEW.md). [Steve Jobs]
