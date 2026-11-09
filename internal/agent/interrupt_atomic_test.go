package agent

import (
	"context"
	"errors"
	"testing"
)

func setInterruptProbe(t *testing.T, thread *Thread, id string, cancelled *bool) {
	t.Helper()
	if askErr := askErr(thread, func(a *actor) error {
		a.state.TurnID = id
		a.autoContinue = true
		*cancelled = false
		flag := cancelled
		a.cancel = func(error) { *flag = true }
		return nil
	}); askErr != nil {
		t.Fatal(askErr)
	}
}

func switchInterruptProbe(t *testing.T, thread *Thread, id string, cancelled *bool) {
	t.Helper()
	if switchErr := askErr(thread, func(a *actor) error {
		a.state.TurnID = id
		a.autoContinue = true
		*cancelled = false
		a.cancel = func(error) { *cancelled = true }
		return nil
	}); switchErr != nil {
		t.Fatal(switchErr)
	}
}

func checkInterruptRefused(t *testing.T, thread *Thread, stale, current string, cancelled *bool) {
	t.Helper()
	if staleErr := thread.InterruptTurn(context.Background(), stale); !errors.Is(staleErr, ErrTurnNotRunning) {
		t.Fatalf("InterruptTurn(stale) = %v, want ErrTurnNotRunning", staleErr)
	}
	if *cancelled {
		t.Fatalf("InterruptTurn(stale) interrupted the new turn")
	}
	observed, observedErr := thread.Status()
	if observedErr != nil {
		t.Fatal(observedErr)
	}
	if observed.Turn != current || !observed.AutoContinue {
		t.Fatalf("refused interrupt changed state: %+v", observed)
	}
}

func TestInterruptTurnIsAtomic(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()

	const oldTurn = "turn-T1"
	const newTurn = "turn-T2"

	var cancelledOld, cancelledNew bool

	setInterruptProbe(t, thread, oldTurn, &cancelledOld)
	observed, statusErr := thread.Status()
	if statusErr != nil || observed.Turn != oldTurn {
		t.Fatalf("status = %+v, %v", observed, statusErr)
	}
	switchInterruptProbe(t, thread, newTurn, &cancelledNew)

	thread.Interrupt()
	if _, flushErr := thread.Status(); flushErr != nil {
		t.Fatal(flushErr)
	}
	if !cancelledNew {
		t.Fatalf("Status then Interrupt did not interrupt the new turn")
	}

	setInterruptProbe(t, thread, newTurn, &cancelledNew)
	checkInterruptRefused(t, thread, oldTurn, newTurn, &cancelledNew)

	if currentErr := thread.InterruptTurn(context.Background(), newTurn); currentErr != nil {
		t.Fatalf("InterruptTurn(current) = %v", currentErr)
	}
	if !cancelledNew {
		t.Fatalf("InterruptTurn(current) did not interrupt the running turn")
	}

	if idleErr := askErr(thread, func(a *actor) error {
		a.state.TurnID = ""
		a.cancel = nil
		return nil
	}); idleErr != nil {
		t.Fatal(idleErr)
	}
	if idleCallErr := thread.InterruptTurn(context.Background(), newTurn); !errors.Is(idleCallErr, ErrTurnNotRunning) {
		t.Fatalf("InterruptTurn(idle) = %v, want ErrTurnNotRunning", idleCallErr)
	}
}
