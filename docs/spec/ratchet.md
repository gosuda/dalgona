# Ratchet

The ratchet gate judges one variant against the best variant so far. This page states its statistic, decision and stop rules.

## Statistic

The gate MUST pair each case of a candidate with the same case of the reference variant.

The gate MUST take the candidate case mean minus the reference case mean for each paired case.

The gate MUST report the mean of the paired differences and a two-sided 95 percent Student t half-width.

The half-width MUST use the critical value for n minus 1 degrees of freedom, and 1.96 above 30 degrees of freedom.

The gate MUST return `up` when the gain minus the half-width is above zero.

The gate MUST return `down` when the gain plus the half-width is below zero.

The gate MUST return `flat` in every other case, and when fewer than two values exist.

For a metric where a lower value is better, the gain MUST be the negated mean.

## Decision

A guardrail whose train or test verdict is `down` MUST revert the round.

A target whose train or test verdict is `down` MUST revert the round.

A target whose train and test verdicts are both `up` MUST keep the round.

A target whose train verdict is `up` and whose test verdict is not `up` MUST revert the round as an overfit suspect.

A target whose test verdict is `up` and whose train verdict is not `up` MUST rerun the round.

Every other round MUST revert as a change outside the noise.

A variant MUST NOT be rerun more than two times. A third rerun decision MUST revert.

## Stop

The flow MUST stop when the reverts since the last keep reach the plateau limit.

The flow MUST stop when the count of gated variant rounds reaches the round limit.

A keep or a baseline round MUST reset the count of reverts since the last keep to zero.

A rerun round MUST leave the count of reverts since the last keep unchanged.

The flow MUST NOT stop while its last round is a rerun.

