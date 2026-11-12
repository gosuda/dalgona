# Reviewed Hashline specifications

Reviewed 2026-09-28 and adopted 2026-11-08. These files specify the `hashline-light` and `hashline-enhanced` edit styles that `internal/tool/patch` implements. Neither is a default.

**Light relaxes prior observation. Enhanced requires prior observation. Both
identify the source snapshot and reject unsupported target changes.**

## Documents

| File | Purpose |
|---|---|
| [Shared specification](hashline-common-v1.md) | Normative syntax, evidence, mapping, lifecycle, authorization, outcomes, memory, replay, and adoption. Read this first. |
| [Light](hashline-light-v1.md) | Precise relaxed condition, risk, tool description, and profile cases. |
| [Enhanced](hashline-enhanced-v1.md) | Prior coverage, delivery cutoffs, strict recovery, and profile cases. |
| [Conformance](hashline-conformance-v1.md) | Acceptance matrix, implementation ownership, proposed product commands, and actual model-check limits. |
| [Review](hashline-review.md) | Findings, revisions, rejected alternatives, source map, and adoption boundaries. |

The review identifies its exact checksum and relevant physical line ranges.

## Executed evidence

[verify_model.py](verify_model.py) and [verification.json](verification.json) contain
reproducible bounded range/gap checks: 109,673 cases plus eight logical regressions.
Run `uv run --no-project --python 3.12 verify_model.py` from `docs/spec/hashline`.

`just hashline-model` fails when the verifier output differs from `verification.json`. When `CI` is `true` and `uv` is missing, `just hashline-model` fails. Otherwise, when `uv` is missing, `just hashline-model` prints `hashline-model: skipped, uv is not installed`. It then exits with status 0.

This is not a filesystem implementation, unrestricted proof, performance benchmark,
or claim that the product tests pass. The conformance document distinguishes
executed checks from required implementation work.

## Adoption rule

Keep legacy styles, aliases, and defaults. Enable the new profiles explicitly.
Enhanced MUST NOT downgrade automatically after refusal. A change to a product default
MUST pass the stated conformance and measured edit-evaluation gates.
