package client

import (
	"context"
	"errors"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type Phase uint8

const (
	PhaseIdle Phase = iota
	PhaseConnecting
	PhaseConnected
	PhaseReconnecting
	PhaseOffline
	PhaseIncompatible
	PhaseFailed
)

const attemptDeadline = 30 * time.Second

var (
	phaseWords  = [...]string{"idle", "connecting", "connected", "reconnecting", "offline", "incompatible", "failed"}
	retryLadder = [...]time.Duration{time.Second, 2 * time.Second, 4 * time.Second}

	errIncompatible  = errors.New("the daemon does not offer the native protocol of this client; update the daemon or this client")
	errTransportLost = errors.New("the connection to the endpoint ended")
)

func (p Phase) String() string {
	if int(p) < len(phaseWords) {
		return phaseWords[p]
	}
	return "unknown"
}

func (p Phase) live() bool {
	return p == PhaseConnecting || p == PhaseConnected || p == PhaseReconnecting
}

type refusal struct{ err error }

func (r refusal) Error() string { return r.err.Error() }

func (r refusal) Unwrap() error { return r.err }

func failurePhase(err error) Phase {
	_, mismatch := errors.AsType[*BuildMismatchError](err)
	data, coded := rpc.ErrorDataOf(err)
	_, refused := errors.AsType[refusal](err)
	switch {
	case mismatch, errors.Is(err, errIncompatible), coded && data.Kind == rpc.ErrorIncompatible:
		return PhaseIncompatible
	case refused, Untrusted(err):
		return PhaseFailed
	}
	return PhaseOffline
}

func sleep(ctx context.Context, delay time.Duration) bool {
	if delay == 0 {
		return ctx.Err() == nil
	}
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-timer.C:
		return true
	case <-ctx.Done():
		return false
	}
}
