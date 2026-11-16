package agent

import (
	"context"
	"fmt"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const (
	maxRecoveryAttempts = 3
	recoveryGaveUp      = "recovery gave up after 3 attempts"
	recoveryHalted      = "agent: recovery halted after 3 attempts"
)

func (a *actor) resumeOpenTurn(ctx context.Context) error {
	turnID := a.state.TurnID
	if turnID == "" || a.damage != nil {
		return nil
	}
	attempts := a.state.RecoveryAttempts(turnID)
	if attempts >= maxRecoveryAttempts {
		return a.abandonTurn(ctx, turnID)
	}
	if _, err := a.appendCtx(ctx, event.TurnRecovery{TurnID: turnID, Attempt: attempts + 1}); err != nil {
		return fmt.Errorf("agent: journal turn.recovery for %s: %w", turnID, err)
	}
	a.spawn(func(ctx context.Context) { (&worker{t: a.t, turnID: turnID}).recoverTurn(ctx) })
	return nil
}

func (a *actor) abandonTurn(ctx context.Context, turnID string) error {
	a.settle(statusFailed)
	completed := event.TurnCompleted{TurnID: turnID, Status: statusFailed, Error: recoveryGaveUp}
	if _, err := a.appendCtx(ctx, completed); err != nil {
		return fmt.Errorf("agent: journal turn.completed for %s: %w", turnID, err)
	}
	a.t.host.log.Warn(recoveryHalted, "thread", a.t.id, "turn", turnID)
	return nil
}
