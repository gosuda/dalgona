package agent

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

var (
	errRecoveryHalted     = errors.New(recoveryHalted)
	errRecoveredPanic     = errors.New("failure: recovered panic")
	errUnformattablePanic = errors.New("panic value could not be formatted")
)

type failureLog struct {
	store   *failure.Store
	sink    func(failure.Record)
	masker  failure.Masker
	product string
	version string
	log     *slog.Logger

	journalMu     sync.Mutex
	journalFailed map[string]bool
}

func newFailureLog(cfg HostConfig, log *slog.Logger) *failureLog {
	out := &failureLog{
		store: cfg.Failures, sink: cfg.FailureSink, product: string(cfg.Product), version: config.Version(), log: log,
		journalFailed: map[string]bool{},
	}
	if cfg.Auth != nil {
		out.masker = cfg.Auth
	}
	return out
}

func (l *failureLog) capture(ctx context.Context, input failure.Input) {
	if l.store == nil {
		return
	}
	input.Product, input.Version = l.product, l.version
	record, err := l.store.Capture(context.WithoutCancel(ctx), input)
	if err != nil {
		l.log.Warn("agent: failure record was not written", "class", input.Class, "error", err)
		return
	}
	l.deliver(record)
}

func (l *failureLog) contain(ctx context.Context, input failure.Input, run func() error) (recovered bool, err error) {
	defer func() {
		if value := recover(); value != nil {
			recovered, err = true, errRecoveredPanic
			l.capturePanic(ctx, value, input)
		}
	}()
	return false, run()
}

func (l *failureLog) capturePanic(ctx context.Context, value any, input failure.Input) {
	input.Class, input.Frames, input.Err = failure.ClassPanic, failure.CaptureFrames(2), panicError(value)
	l.capture(ctx, input)
}

func panicError(value any) (err error) {
	defer func() {
		if recover() != nil {
			err = errUnformattablePanic
		}
	}()
	return errors.New(fmt.Sprint(value))
}

func (l *failureLog) deliver(record failure.Record) {
	if l.sink != nil {
		l.sink(record)
	}
}

func (l *failureLog) firstJournalFailure(session string) bool {
	l.journalMu.Lock()
	defer l.journalMu.Unlock()
	if l.journalFailed[session] {
		return false
	}
	l.journalFailed[session] = true
	return true
}

func (t *Thread) failureOrigin() failure.Origin {
	if t.created.Advisor != "" {
		return failure.OriginSide
	}
	return failure.OriginMain
}

func unavailableStep(reason string) *failure.FailureStep {
	step := failure.UnavailableStep(reason)
	return &step
}

func (w *worker) correlation(callID string) failure.Correlation {
	return failure.Correlation{Session: w.t.id, Turn: w.turnID, CallID: callID}
}

func (w *worker) turnPanic() failure.Input {
	return failure.Input{Origin: w.t.failureOrigin(), Correlation: w.correlation(""), FailingStep: unavailableStep(failure.ReasonPanicNoCall)}
}

func (w *worker) containedLoop(ctx context.Context) outcome {
	o := outcome{status: statusFailed, failure: errRecoveredPanic.Error()}
	_, _ = w.t.host.failures.contain(ctx, w.turnPanic(), func() error {
		o = w.loop(ctx)
		return nil
	})
	return o
}

func (w *worker) containedPrime(ctx context.Context) error {
	_, err := w.t.host.failures.contain(ctx, w.turnPanic(), func() error { return w.prime(ctx) })
	return err
}

func (a *actor) panicked(fn func(*actor)) (recovered bool) {
	defer func() {
		if value := recover(); value != nil {
			recovered = true
			a.t.host.failures.capturePanic(context.Background(), value, failure.Input{
				Origin: a.t.failureOrigin(), Correlation: failure.Correlation{Session: a.t.id, Turn: a.state.TurnID},
				FailingStep: unavailableStep(failure.ReasonPanicNoCall),
			})
		}
	}()
	fn(a)
	return false
}

func (w *worker) invoke(ctx context.Context, p *prepared) (tool.Result, error) {
	var result tool.Result
	recovered, err := w.t.host.failures.contain(ctx, w.callFailure(p, failure.ClassPanic, unavailableStep(failure.ReasonPanicNoCall)), func() error {
		var runErr error
		result, runErr = w.dispatch(ctx, p)
		return runErr
	})
	if recovered {
		p.discardPlan()
		return tool.Result{Failure: tool.FailureInternal}, err
	}
	if result.Failure == tool.FailureInternal {
		input := w.callFailure(p, failure.ClassTool, w.replayStep(ctx, p))
		input.Err, input.Output = err, string(result.Output)
		w.t.host.failures.capture(ctx, input)
	}
	return result, err
}

func (w *worker) callFailure(p *prepared, class failure.Class, step *failure.FailureStep) failure.Input {
	return failure.Input{
		Class: class, Tool: p.call.Name, Args: string(p.args), Workspace: w.t.created.Cwd,
		Origin: w.t.failureOrigin(), Correlation: w.correlation(p.call.ID), FailingStep: step,
	}
}

func (w *worker) replayStep(ctx context.Context, p *prepared) *failure.FailureStep {
	step, err := failure.BuildFailureStep(context.WithoutCancel(ctx), w.t.host.failures.masker, failure.ConvertInput{
		Tool: p.call.Name, Freeform: p.call.Freeform, OrigArgs: p.args, Workspace: w.t.created.Cwd,
		Completed: true, Unavailable: providerCallState(p.call),
	})
	if err != nil {
		return unavailableStep(failure.ReasonTiming)
	}
	return &step
}

func providerCallState(call event.ToolCall) string {
	if call.Signature != "" || len(call.RedactedThinking) > 0 {
		return failure.ReasonProviderState
	}
	return ""
}

func (w *worker) captureModelFailure(ctx context.Context, err error) {
	if ctx.Err() != nil || !terminalModelFailure(err) {
		return
	}
	input := failure.Input{
		Class: failure.ClassModel, Err: err, Origin: w.t.failureOrigin(), Correlation: w.correlation(""),
		FailingStep: unavailableStep(failure.ReasonProvider),
	}
	if panicked, ok := providerPanic(err); ok {
		input.Class, input.Err, input.Frames = failure.ClassPanic, errors.New(panicked.Value), panicked.Frames
		input.FailingStep = unavailableStep(failure.ReasonPanicNoCall)
	}
	w.t.host.failures.capture(ctx, input)
}

func providerPanic(err error) (*model.ProviderPanic, bool) {
	failed, ok := errors.AsType[modelFailure](err)
	if !ok {
		return nil, false
	}
	return errors.AsType[*model.ProviderPanic](failed.failure.Err)
}

func terminalModelFailure(err error) bool {
	_, provider := errors.AsType[modelFailure](err)
	return provider || errors.Is(err, errEmptyResponse) || errors.Is(err, errOverflowTwice)
}

func (a *actor) captureJournalFailure(ctx context.Context, err error) {
	if ctx.Err() != nil || !a.t.host.failures.firstJournalFailure(a.t.id) {
		return
	}
	a.t.host.failures.capture(ctx, failure.Input{
		Class: failure.ClassJournal, Err: err, Origin: a.t.failureOrigin(),
		Correlation: failure.Correlation{Session: a.t.id, Turn: a.state.TurnID}, FailingStep: unavailableStep(failure.ReasonEnv),
	})
}

func (a *actor) captureRecoveryHalt(ctx context.Context, turnID string) {
	a.t.host.failures.capture(ctx, failure.Input{
		Class: failure.ClassRuntime, Err: errRecoveryHalted, Origin: a.t.failureOrigin(),
		Correlation: failure.Correlation{Session: a.t.id, Turn: turnID}, FailingStep: unavailableStep(failure.ReasonSandbox),
	})
}
