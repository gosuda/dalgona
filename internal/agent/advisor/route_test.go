package advisor_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/advisor"
)

func TestRoute_matrix(t *testing.T) {
	t.Parallel()
	const (
		running     = advisor.PhaseRunning
		idle        = advisor.PhaseIdle
		interrupted = advisor.PhaseInterrupted
		nit         = advisor.SeverityNit
		concern     = advisor.SeverityConcern
		blocker     = advisor.SeverityBlocker
		steer       = advisor.DeliverySteer
		aside       = advisor.DeliveryAside
		resume      = advisor.DeliveryContinue
	)
	policy := advisor.Policy{ContinueSeverity: concern, ImmuneTurns: 3}
	outside := advisor.PrimaryState{Steered: true, TurnsSinceSteer: 3}
	inside := advisor.PrimaryState{Steered: true, TurnsSinceSteer: 2}
	cases := []struct {
		phase    advisor.Phase
		severity advisor.Severity
		immune   bool
		want     advisor.Delivery
	}{
		{running, nit, false, aside},
		{running, concern, false, steer},
		{running, blocker, false, steer},
		{running, nit, true, aside},
		{running, concern, true, aside},
		{running, blocker, true, steer},
		{idle, nit, false, aside},
		{idle, concern, false, resume},
		{idle, blocker, false, resume},
		{idle, nit, true, aside},
		{idle, concern, true, aside},
		{idle, blocker, true, resume},
		{interrupted, nit, false, aside},
		{interrupted, concern, false, aside},
		{interrupted, blocker, false, aside},
		{interrupted, nit, true, aside},
		{interrupted, concern, true, aside},
		{interrupted, blocker, true, aside},
	}
	for _, test := range cases {
		state := outside
		if test.immune {
			state = inside
		}
		state.Phase = test.phase
		if got := advisor.Route(state, test.severity, policy); got != test.want {
			t.Errorf("Route(phase %d, %s, immune %v) = %s, want %s", test.phase, test.severity, test.immune, got, test.want)
		}
	}
}

func TestRoute_policyEdges(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name     string
		state    advisor.PrimaryState
		severity advisor.Severity
		policy   advisor.Policy
		want     advisor.Delivery
	}{
		{name: "default continue severity keeps idle concern aside", state: advisor.PrimaryState{Phase: advisor.PhaseIdle}, severity: advisor.SeverityConcern, policy: advisor.Policy{ContinueSeverity: advisor.SeverityBlocker, ImmuneTurns: 3}, want: advisor.DeliveryAside},
		{name: "default continue severity continues idle blocker", state: advisor.PrimaryState{Phase: advisor.PhaseIdle}, severity: advisor.SeverityBlocker, policy: advisor.Policy{ContinueSeverity: advisor.SeverityBlocker, ImmuneTurns: 3}, want: advisor.DeliveryContinue},
		{name: "nit continue severity continues idle nit", state: advisor.PrimaryState{Phase: advisor.PhaseIdle}, severity: advisor.SeverityNit, policy: advisor.Policy{ContinueSeverity: advisor.SeverityNit}, want: advisor.DeliveryContinue},
		{name: "one continuation per batch", state: advisor.PrimaryState{Phase: advisor.PhaseIdle, Continued: true}, severity: advisor.SeverityBlocker, policy: advisor.Policy{ContinueSeverity: advisor.SeverityNit}, want: advisor.DeliveryAside},
		{name: "no steer yet means no immune window", state: advisor.PrimaryState{Phase: advisor.PhaseRunning}, severity: advisor.SeverityConcern, policy: advisor.Policy{ImmuneTurns: 3}, want: advisor.DeliverySteer},
		{name: "zero immune turns never demotes", state: advisor.PrimaryState{Phase: advisor.PhaseRunning, Steered: true}, severity: advisor.SeverityConcern, policy: advisor.Policy{}, want: advisor.DeliverySteer},
		{name: "immune window covers the steer turn", state: advisor.PrimaryState{Phase: advisor.PhaseRunning, Steered: true}, severity: advisor.SeverityConcern, policy: advisor.Policy{ImmuneTurns: 1}, want: advisor.DeliveryAside},
		{name: "immune window ends after N turns", state: advisor.PrimaryState{Phase: advisor.PhaseRunning, Steered: true, TurnsSinceSteer: 1}, severity: advisor.SeverityConcern, policy: advisor.Policy{ImmuneTurns: 1}, want: advisor.DeliverySteer},
	}
	for _, test := range cases {
		if got := advisor.Route(test.state, test.severity, test.policy); got != test.want {
			t.Errorf("%s: Route = %s, want %s", test.name, got, test.want)
		}
	}
}
