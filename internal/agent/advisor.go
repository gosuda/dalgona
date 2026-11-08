package agent

import (
	"context"
	"errors"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func (t *Thread) SetBoundaryHook(hook BoundaryHook) error {
	return askErr(t, func(a *actor) error {
		a.hook = hook
		return nil
	})
}

type deliverResult struct {
	delivery Delivery
	err      error
}

func (t *Thread) Deliver(ctx context.Context, note Note) (Delivery, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	result, err := ask(t, func(a *actor) deliverResult { return a.deliver(note) })
	return result.delivery, errors.Join(err, result.err)
}

func (a *actor) deliver(note Note) deliverResult {
	if a.damage != nil {
		return deliverResult{err: ErrDamaged}
	}
	journaled := event.AdvisorNote{
		Advisor: note.Advisor, Severity: note.Severity, Note: note.Text, ReviewSeq: note.ReviewSeq,
		Delivery: string(a.effectiveDelivery(note.Delivery)),
	}
	if seq, ok := a.state.Notes[event.NoteKey(journaled)]; ok {
		recorded, _ := a.records[seq-1].Event.(event.AdvisorNote)
		return deliverResult{delivery: Delivery(recorded.Delivery)}
	}
	if journaled.Delivery != string(DeliveryContinue) {
		_, err := a.append(journaled)
		return deliverResult{delivery: Delivery(journaled.Delivery), err: err}
	}
	turnID := newID("turn-")
	if _, err := a.append(journaled, event.TurnStarted{TurnID: turnID}); err != nil {
		return deliverResult{err: err}
	}
	a.spawnContinuation(turnID)
	return deliverResult{delivery: DeliveryContinue}
}

func (a *actor) effectiveDelivery(requested Delivery) Delivery {
	switch {
	case requested == DeliveryAside || !a.autoContinue:
		return DeliveryAside
	case a.busy && a.state.TurnID != "":
		return DeliverySteer
	case requested == DeliveryContinue && !a.busy:
		return DeliveryContinue
	default:
		return DeliveryAside
	}
}

func (t *Thread) ReadAfter(seq uint64) (Read, error) {
	return ask(t, func(a *actor) Read { return a.readAfter(seq) })
}

func (a *actor) readAfter(seq uint64) Read {
	out := Read{Head: a.head()}
	for _, item := range a.records[min(seq, a.head()):] {
		switch item.Event.(type) {
		case event.Compaction, event.Branch:
			out.Reset = true
		}
	}
	path := event.ActivePath(a.records)
	if out.Reset {
		out.Events = afterCompactionMarker(path)
		return out
	}
	for _, item := range path {
		if item.Seq > seq {
			out.Events = append(out.Events, item)
		}
	}
	return out
}

func afterCompactionMarker(path []event.Seqd) []event.Seqd {
	for i := len(path) - 1; i >= 0; i-- {
		marker, ok := path[i].Event.(event.Compaction)
		if !ok {
			continue
		}
		kept := make([]event.Seqd, 0, len(path))
		for _, item := range path {
			if item.Seq >= marker.FirstKeptSeq {
				kept = append(kept, item)
			}
		}
		return kept
	}
	return path
}
