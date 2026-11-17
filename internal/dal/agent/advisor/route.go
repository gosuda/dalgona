package advisor

type Phase uint8

const (
	PhaseRunning Phase = iota + 1
	PhaseIdle
	PhaseInterrupted
)

type Delivery string

const (
	DeliverySteer    Delivery = "steer"
	DeliveryAside    Delivery = "aside"
	DeliveryContinue Delivery = "continue"
)

type PrimaryState struct {
	Phase           Phase
	Steered         bool
	TurnsSinceSteer int
	Continued       bool
}

type Policy struct {
	ContinueSeverity Severity
	ImmuneTurns      int
}

func Route(state PrimaryState, severity Severity, policy Policy) Delivery {
	if severity != SeverityBlocker && state.immune(policy.ImmuneTurns) {
		return DeliveryAside
	}
	switch state.Phase {
	case PhaseRunning:
		if severity >= SeverityConcern {
			return DeliverySteer
		}
	case PhaseIdle:
		if !state.Continued && severity >= policy.ContinueSeverity {
			return DeliveryContinue
		}
	case PhaseInterrupted:
	}
	return DeliveryAside
}

func (s PrimaryState) immune(turns int) bool {
	return s.Steered && s.TurnsSinceSteer < turns
}
