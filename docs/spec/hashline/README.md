# Reviewed Hashline specifications

Reviewed proposal, 2026-09-28. These files specify two optional new profiles for
dal's existing patch engine. They do not implement the feature or change defaults.

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
Run with `uv run --no-project --python 3.12 verify_model.py`.

This is not a filesystem implementation, unrestricted proof, performance benchmark,
or claim that the product tests pass. The conformance document distinguishes
executed checks from required implementation work.

[package-checks.json](package-checks.json) records document/link/source-hash checks.
[SHA256SUMS](SHA256SUMS) records the packaged file checksums, excluding itself.

## Adoption rule

Keep legacy styles, aliases, and defaults. Enable the new profiles explicitly.
Never downgrade Enhanced automatically after refusal. Changing a product default
requires the stated conformance and measured edit-evaluation gates.
