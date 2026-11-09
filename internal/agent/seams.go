package agent

import (
	"context"

	"github.com/gosuda/dalgona/internal/agent/event"
)

type BoundaryKind int

const (
	BoundaryTurn BoundaryKind = iota + 1
	BoundaryAgentEnd
)

type Boundary struct {
	Thread       string
	Turn         string
	Kind         BoundaryKind
	Head         uint64
	Continuation bool
	Ended        string
}

type Wait func(ctx context.Context) error

type BoundaryHook func(b Boundary) Wait

type Delivery string

const (
	DeliverySteer    Delivery = "steer"
	DeliveryAside    Delivery = "aside"
	DeliveryContinue Delivery = "continue"
)

type Note struct {
	Advisor   string
	Severity  string
	Text      string
	ReviewSeq uint64
	Delivery  Delivery
}

type Read struct {
	Events []event.Seqd
	Head   uint64
	Reset  bool
}

type Status struct {
	State         event.Status
	Busy          bool
	AutoContinue  bool
	Head          uint64
	Turn          string
	Subscribers   int
	OutputDropped uint64
}
