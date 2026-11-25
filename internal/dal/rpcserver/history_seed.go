package rpcserver

import (
	"context"
	"errors"
	"io"
	"iter"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func (w *watch) seedProjection(ctx context.Context, seed iter.Seq2[agent.Update, error]) error {
	w.apply.Lock()
	defer w.apply.Unlock()
	if w.live.closed {
		return io.ErrClosedPipe
	}
	for update, err := range seed {
		if ctxErr := ctx.Err(); ctxErr != nil {
			return ctxErr
		}
		switch {
		case errors.Is(err, agent.ErrActiveSpillLimit):
			return rpc.NativeKind(rpc.ErrorLimitExceeded)
		case err != nil:
			return rpc.NativeKind(rpc.ErrorUnavailable)
		}
		w.seedDelta(update)
	}
	return nil
}

func (w *watch) seedDelta(update agent.Update) {
	item := w.seedItem(update)
	if item == nil {
		return
	}
	w.live.begin(item, w.tracker.currentTurn())
	w.live.append(item.ID, update.Delta)
}

func (w *watch) seedItem(update agent.Update) *rpc.Item {
	switch update.Kind {
	case agent.UpdateTextDelta:
		return w.tracker.seedStream(rpc.KindAgent, update.ItemID)
	case agent.UpdateReasoningDelta:
		return w.tracker.seedStream(rpc.KindReasoning, update.ItemID+reasoningSuffix)
	case agent.UpdateExecOutput:
		if item := w.tracker.items[update.CallID]; item != nil && item.Type == rpc.KindCommand {
			return item
		}
	case agent.UpdateEvent, agent.UpdateApproval, agent.UpdateRetryWait, agent.UpdateQuestion, agent.UpdateWarning:
	}
	return nil
}

func (t *tracker) seedStream(kind, id string) *rpc.Item {
	item := t.items[id]
	if item == nil {
		item = &rpc.Item{Type: kind, ID: id}
		t.items[id] = item
	}
	return item
}
