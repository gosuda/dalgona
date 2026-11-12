# Ratchet

## Invariant

The host MUST own the split, the approvals, the completeness and leak checks, and the round decision of each ratchet flow. The `dal` product MUST NOT import `internal/ratchet`.

## Scope

This decision covers `internal/ratchet`, the battery wiring in `internal/app/battery`, and the depguard rules in `.golangci.yml`. The statistic and decision rules are in [ratchet.md](../spec/ratchet.md). The ported rules keep their MIT notice in [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md).

## Enforcement

The depguard `core` rule rejects an import of `internal/ratchet` from core packages. The depguard `dal` rule rejects an import of it from `cmd/dal`. The tests `TestMeanAndHalfAndVerdict`, `TestDecideRound`, `TestPlateauAfterReverts` and `TestRoundCeilingReplacesRerun` in `internal/ratchet` check the statistic and decision rules.
