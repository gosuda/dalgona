package script

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"go.starlark.net/resolve"
	"go.starlark.net/starlark"
	"go.starlark.net/syntax"

	"github.com/gosuda/dalgona/internal/dal/script/stdlib"
)

const (
	maxSteps       = 1_000_000_000
	maxPrintLines  = 1000
	maxPrintBytes  = 64 << 10
	resultGlobal   = "$result"
	contextLocal   = "dalgona.context"
	stageArgs      = "args"
	stageParse     = "parse"
	stageUses      = "uses"
	stageRun       = "run"
	stageResult    = "result"
	codeError      = "error"
	codeSyntax     = "syntax"
	codeInvalid    = "invalid_args"
	codeScope      = "scope_exceeded"
	codeDenied     = "denied"
	codeCancelled  = "cancelled"
	codeLimit      = "limit_exceeded"
	codeIndet      = "indeterminate"
	codeNondet     = "nondeterminism"
	codeLoadEffect = "load_effect"
	codeJournal    = "journal"
	codeResult     = "result"
	msgLoadEffect  = "effects are not allowed at load time"
)

type stopError struct {
	status Status
	code   string
	msg    string
}

func (e *stopError) Error() string { return e.msg }

type parseError struct{ err error }

func (e *parseError) Error() string { return e.err.Error() }
func (e *parseError) Unwrap() error { return e.err }

type nameSet map[string]struct{}

func newNameSet(names []string) nameSet {
	set := make(nameSet, len(names))
	for _, name := range names {
		set[name] = struct{}{}
	}
	return set
}

func (s nameSet) has(name string) bool {
	if s == nil {
		return true
	}
	_, ok := s[name]
	return ok
}

type runConfig struct {
	reach nameSet
	allow nameSet
}

type Runtime struct {
	fx       Effects
	id       string
	cfg      runConfig
	thread   *starlark.Thread
	cancel   context.CancelCauseFunc
	next     int
	calls    int
	loading  bool
	mu       sync.Mutex
	fatal    *stopError
	inflight sync.WaitGroup
	prints   Prints
	printed  int
}

func newRun(fx Effects, id string, cfg runConfig) *Runtime {
	r := &Runtime{fx: fx, id: id, cfg: cfg, cancel: func(error) {}}
	r.thread = &starlark.Thread{Name: id, Print: r.print}
	r.thread.SetMaxExecutionSteps(maxSteps)
	r.thread.OnMaxSteps = func(*starlark.Thread) {
		_ = r.halt(&stopError{status: StatusLimitExceeded, code: codeLimit, msg: fmt.Sprintf("the script exceeded %d execution steps", maxSteps)})
	}
	return r
}

func NewSeed() int64 {
	var buf [8]byte
	_, _ = rand.Read(buf[:])
	return int64(binary.LittleEndian.Uint64(buf[:]) >> 1)
}

func NewRuntime(ctx context.Context, fx Effects, id string, seed int64) (*Runtime, starlark.StringDict, func()) {
	r := newRun(fx, id, runConfig{})
	env, end := r.begin(ctx, seed)
	return r, env, end
}

func (r *Runtime) LoadCell(ctx context.Context, entry string, src []byte, env starlark.StringDict) (starlark.Value, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	stop := context.AfterFunc(ctx, func() { r.thread.Cancel("cancelled") })
	defer stop()
	r.loading = true
	defer func() { r.loading = false }()
	value, err := r.ExecCell(ctx, entry, src, env)
	if fatal := r.fatalStop(); fatal != nil {
		return nil, fatal
	}
	return value, err
}

func (r *Runtime) Call(ctx context.Context, impl starlark.Callable, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	stop := context.AfterFunc(ctx, func() { r.thread.Cancel("cancelled") })
	defer stop()
	return starlark.Call(r.thread, impl, args, kwargs)
}

func (r *Runtime) begin(parent context.Context, seed int64) (starlark.StringDict, func()) {
	ctx, cancel := context.WithCancelCause(parent)
	r.cancel = cancel
	r.thread.SetLocal(contextLocal, ctx)
	stopAfter := context.AfterFunc(ctx, func() { r.thread.Cancel("cancelled") })
	env := stdlib.Predeclared(stdlib.Hooks{
		Now:   func() float64 { return r.now(ctx) },
		Sleep: func(d time.Duration) error { return r.sleepFor(ctx, d) },
		Seed:  seed,
	})
	return env, func() {
		stopAfter()
		r.inflight.Wait()
		cancel(nil)
	}
}

func (r *Runtime) WithContext(env starlark.StringDict) starlark.StringDict {
	env["ctx"] = r.ContextValue()
	env["tools"] = &toolsValue{r: r}
	return env
}

func runContext(thread *starlark.Thread) context.Context {
	if ctx, ok := thread.Local(contextLocal).(context.Context); ok {
		return ctx
	}
	return context.Background()
}

func (r *Runtime) print(_ *starlark.Thread, msg string) {
	for line := range strings.SplitSeq(msg, "\n") {
		if len(r.prints.Lines) >= maxPrintLines || r.printed+len(line) > maxPrintBytes {
			r.prints.Truncated = true
			return
		}
		r.printed += len(line)
		r.prints.Lines = append(r.prints.Lines, line)
	}
}

func (r *Runtime) halt(err error) error {
	stop, ok := errors.AsType[*stopError](err)
	if !ok {
		stop = &stopError{status: StatusFailed, code: codeError, msg: err.Error()}
	}
	r.mu.Lock()
	if r.fatal == nil {
		r.fatal = stop
	}
	r.mu.Unlock()
	r.thread.Cancel(stop.msg)
	r.cancel(stop)
	return stop
}

func (r *Runtime) fatalStop() *stopError {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.fatal
}

func (r *Runtime) submit(op string, args jsontext.Value) (Slot, error) {
	if r.loading {
		return Slot{}, r.halt(&stopError{status: StatusFailed, code: codeLoadEffect, msg: msgLoadEffect})
	}
	r.next++
	return Slot{Run: r.id, N: r.next, Op: op, ArgsSHA: ArgsSHA(args)}, nil
}

func (r *Runtime) resolve(ctx context.Context, slot Slot, live effectFunc) (jsontext.Value, error) {
	rec, ok := r.fx.Lookup(slot.Run, slot.N)
	if !ok {
		value, err := live(ctx)
		if err != nil {
			return nil, r.effectError(ctx, err)
		}
		return value, nil
	}
	if rec.Op != slot.Op || rec.ArgsSHA != slot.ArgsSHA {
		return nil, r.halt(&stopError{status: StatusFailed, code: codeNondet, msg: fmt.Sprintf("nondeterminism at effect %d: recorded %s, got %s", slot.N, rec.Op, slot.Op)})
	}
	if rec.Interrupted {
		return nil, r.halt(&stopError{status: StatusIndeterminate, code: codeIndet, msg: fmt.Sprintf("effect %d (%s) was interrupted; it may have partly run", slot.N, slot.Op)})
	}
	if rec.Denied {
		return nil, r.effectError(ctx, fmt.Errorf("%w: %s", ErrDenied, rec.Error))
	}
	if rec.Error != "" {
		return nil, errors.New(rec.Error)
	}
	return rec.Result, nil
}

func (r *Runtime) cancelled(ctx context.Context) error {
	cause := context.Cause(ctx)
	if stop, ok := errors.AsType[*stopError](cause); ok {
		return stop
	}
	return &stopError{status: StatusCancelled, code: codeCancelled, msg: "cancelled: " + cause.Error()}
}

func (r *Runtime) record(slot Slot, value jsontext.Value) error {
	if err := r.fx.Record(slot, value); err != nil {
		return &stopError{status: StatusFailed, code: codeJournal, msg: fmt.Sprintf("journal effect %d: %v", slot.N, err)}
	}
	return nil
}

func (r *Runtime) ExecCell(ctx context.Context, entry string, src []byte, env starlark.StringDict) (starlark.Value, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	stop := context.AfterFunc(ctx, func() { r.thread.Cancel("cancelled") })
	defer stop()
	opts := stdlib.FileOptions()
	file, err := opts.Parse(entry, src, 0)
	if err != nil {
		return nil, &parseError{err: err}
	}
	captureLast(file)
	prog, err := starlark.FileProgram(file, env.Has)
	if err != nil {
		return nil, &parseError{err: err}
	}
	globals, err := prog.Init(r.thread, env)
	if err != nil {
		return nil, err
	}
	if value, ok := globals[resultGlobal]; ok && value != nil {
		return value, nil
	}
	return starlark.None, nil
}

func captureLast(file *syntax.File) {
	last := len(file.Stmts) - 1
	if last < 0 {
		return
	}
	expr, ok := file.Stmts[last].(*syntax.ExprStmt)
	if !ok {
		return
	}
	start, _ := expr.Span()
	file.Stmts[last] = &syntax.AssignStmt{OpPos: start, Op: syntax.EQ, LHS: &syntax.Ident{NamePos: start, Name: resultGlobal}, RHS: expr.X}
}

func (r *Runtime) Finish(parent context.Context, value starlark.Value, err error) Outcome {
	r.inflight.Wait()
	if err != nil || r.fatalStop() != nil {
		return Outcome{Status: r.status(parent, err), Error: r.failure(parent, err), Prints: r.prints}
	}
	encoded, encErr := ToJSON(value)
	if encErr != nil {
		return Outcome{Status: StatusFailed, Error: Failure{Code: codeResult, Message: "result is not JSON: " + encErr.Error(), Stage: stageResult}, Prints: r.prints}
	}
	return Outcome{Status: StatusCompleted, Value: encoded, Prints: r.prints}
}

func (r *Runtime) status(parent context.Context, err error) Status {
	if stop := r.classify(parent, err); stop != nil {
		return stop.status
	}
	return StatusFailed
}

func (r *Runtime) failure(parent context.Context, err error) Failure {
	if stop := r.classify(parent, err); stop != nil {
		return Failure{Code: stop.code, Message: stop.msg, Stage: stageRun}
	}
	if parse, ok := errors.AsType[*parseError](err); ok {
		return Failure{Code: codeSyntax, Message: syntaxMessage(parse.err), Stage: stageParse}
	}
	return Failure{Code: codeError, Message: err.Error(), Stage: stageRun}
}

func (r *Runtime) classify(parent context.Context, err error) *stopError {
	if stop := r.fatalStop(); stop != nil {
		return stop
	}
	if parent.Err() != nil {
		return &stopError{status: StatusCancelled, code: codeCancelled, msg: "cancelled: " + context.Cause(parent).Error()}
	}
	if stop, ok := errors.AsType[*stopError](err); ok {
		return stop
	}
	return nil
}

func syntaxMessage(err error) string {
	if list, ok := errors.AsType[resolve.ErrorList](err); ok {
		lines := make([]string, len(list))
		for i, item := range list {
			lines[i] = item.Error()
		}
		return strings.Join(lines, "\n")
	}
	return err.Error()
}

func stopped(status Status, code, msg, stage string) Outcome {
	return Outcome{Status: status, Error: Failure{Code: code, Message: msg, Stage: stage}}
}
