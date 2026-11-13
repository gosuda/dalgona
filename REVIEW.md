# Review

## Report defects, not preferences

Each finding MUST name the defect, its location, the affected consumer, and the consequence. It MUST quote or cite the evidence. Severity MUST describe the harm in plain words. A review MUST NOT invent a failure or claim that an unrun check passed.

A finding SHOULD include a test that fails on the defect. It MUST use a declared check from [justfile](justfile), a focused Go test, or a web script from [web/package.json](web/package.json). A saved-output pin or source-text assertion MUST NOT count as proof. The one exception is the comment gate that `CONTRIBUTE.md` names.

## Check the authority boundaries

A journal change MUST preserve raw event bytes, sequence order, CRC, and the hash chain. It MUST NOT silently discard a committed event. A damaged session MUST remain read-only. Use [journal.md](docs/spec/journal.md) and the tests in `internal/journal`.

A recovery change MUST classify each pending tool call under [recovery.md](docs/spec/recovery.md). It MUST NOT rerun an unknown command outcome as if the command never ran. A repeated runner call id MUST NOT create a second process. Use [runner.md](docs/spec/runner.md) and the recovery tests in `internal/agent`.

A protocol change MUST preserve request ids, item ids, notification order, and the Codex schema. The review MUST inspect both the server response and its client consumer. Use [protocol.md](docs/spec/protocol.md) and `internal/rpc/testdata/codex-schema`.

An approval change MUST preserve the gate order in [approvals.md](docs/spec/approvals.md). A static refusal MUST NOT reach the automated decider. An advisor note MUST NOT approve an action. Use [advisor.md](docs/spec/advisor.md) for the advisor boundary.

A secret-handling change MUST keep raw secret values out of model input and the journal. It MUST preserve restart-safe placeholder mappings. Unmasking MUST remain at the exec and patch boundary. Use [secrets.md](docs/spec/secrets.md).

The local harness sandbox MUST NOT be described as a security boundary. An unavailable sandbox MUST NOT be reported as active. Use [sandbox.md](docs/spec/sandbox.md).

## Reject unnecessary structure

The review MUST flag a forbidden import, a dependency cycle, a pass-through layer, or an interface with no required seam. [.golangci.yml](.golangci.yml) defines the package boundaries and import bans.

The review MUST flag dead code, unused exports, placeholder paths, duplicate implementations, commented-out code, and temporary debug output. It MUST also flag a partial caller migration or an obsolete compatibility path. A line-count goal MUST NOT justify removal of a required error path, evidence clause, or test oracle.

## Record the result

The review MUST state which defects remain and which checks supplied evidence. It MUST distinguish a passing test from inspection and from an untested claim. Approval MUST NOT imply that live credentials, host reboot, the Codex desktop app, or a cloud service were exercised when they were not.
