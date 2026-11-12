package agent

import (
	"time"

	"github.com/gosuda/dalgona/internal/model"
)

func (w *worker) retryWatch() model.RetryWatch {
	return model.RetryWatch{
		Attended: func() bool {
			attached, err := ask(w.t, func(a *actor) bool { return len(a.subs) > 0 })
			if err != nil {
				w.t.host.log.Debug("agent: retry wait attendance", "thread", w.t.id, "error", err)
				return false
			}
			return attached
		},
		Tick: func(remaining time.Duration) {
			if _, err := ask(w.t, func(a *actor) bool {
				a.publish(Update{Kind: UpdateRetryWait, Thread: a.t.id, Turn: w.turnID, Wait: remaining})
				return true
			}); err != nil {
				w.t.host.log.Debug("agent: retry wait tick", "thread", w.t.id, "remaining", remaining, "error", err)
			}
		},
	}
}
