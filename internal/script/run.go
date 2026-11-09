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

	"github.com/gosuda/dalgona/internal/script/stdlib"
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

type run struct {
	fx       Effects
	id       string
	cfg      runConfig
	thread   *starlark.Thread
	cancel   context.CancelCauseFunc
	next     int
	loading  bool
	mu       sync.Mutex
	fatal    *stopError
	inflight sync.WaitGroup
	prints   Prints
	printed  int
}

const DeniedMark = "denied: "

func newRun(fx Effects, id string, cfg runConfig) *run {
	r := &run{fx: fx, id: id, cfg: cfg, cancel: func(error) {}}
	r.thread = &starlark.Thread{Name: id, Print: r.print}
	r.thread.SetMaxExecutionSteps(maxSteps)
	r.thread.OnMaxSteps = func(*starlark.Thread) {
		_ = r.halt(&stopError{status: StatusLimitExceeded, code: codeLimit, msg: fmt.Sprintf("the script exceeded %d execution steps", maxSteps)})
	}
	return r
}

func newSeed() int64 {
	var buf [8]byte
	_, _ = rand.Read(buf[:])
	return int64(binary.LittleEndian.Uint64(buf[:]) >> 1)
}

func (r *run) begin(parent context.Context, seed int64) (starlark.StringDict, func()) {
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

func (r *run) withContext(env starlark.StringDict) starlark.StringDict {
	env["ctx"] = r.ctxValue()
	env["tools"] = &toolsValue{r: r}
	return env
}

func runContext(thread *starlark.Thread) context.Context {
	if ctx, ok := thread.Local(contextLocal).(context.Context); ok {
		return ctx
	}
	return context.Background()
}

func (r *run) print(_ *starlark.Thread, msg string) {
	for line := range strings.SplitSeq(msg, "\n") {
		if len(r.prints.Lines) >= maxPrintLines || r.printed+len(line) > maxPrintBytes {
			r.prints.Truncated = true
			return
		}
		r.printed += len(line)
		r.prints.Lines = append(r.prints.Lines, line)
	}
}

func (r *run) halt(err error) error {
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

func (r *run) fatalStop() *stopError {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.fatal
}

func (r *run) submit(op string, args jsontext.Value) (Slot, error) {
	if r.loading {
		return Slot{}, r.halt(&stopError{status: StatusFailed, code: codeLoadEffect, msg: msgLoadEffect})
	}
	r.next++
	return Slot{Run: r.id, N: r.next, Op: op, ArgsSHA: ArgsSHA(args)}, nil
}

func (r *run) resolve(ctx context.Context, slot Slot, live effectFunc) (jsontext.Value, error) {
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
	if rec.Error != "" {
		if msg, ok := strings.CutPrefix(rec.Error, DeniedMark); ok {
			return nil, r.effectError(ctx, fmt.Errorf("%w: %s", ErrDenied, msg))
		}
		return nil, errors.New(rec.Error)
	}
	return rec.Result, nil
}

func (r *run) cancelled(ctx context.Context) error {
	cause := context.Cause(ctx)
	if stop, ok := errors.AsType[*stopError](cause); ok {
		return stop
	}
	return &stopError{status: StatusCancelled, code: codeCancelled, msg: "cancelled: " + cause.Error()}
}

func (r *run) record(slot Slot, value jsontext.Value) error {
	if err := r.fx.Record(slot, value); err != nil {
		return &stopError{status: StatusFailed, code: codeJournal, msg: fmt.Sprintf("journal effect %d: %v", slot.N, err)}
	}
	return nil
}

func (r *run) execCell(entry string, src []byte, env starlark.StringDict) (starlark.Value, error) {
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

func (r *run) finish(parent context.Context, value starlark.Value, err error) Outcome {
	r.inflight.Wait()
	if err != nil || r.fatalStop() != nil {
		return Outcome{Status: r.status(parent, err), Error: r.failure(parent, err), Prints: r.prints}
	}
	encoded, encErr := toJSON(value)
	if encErr != nil {
		return Outcome{Status: StatusFailed, Error: Failure{Code: codeResult, Message: "result is not JSON: " + encErr.Error(), Stage: stageResult}, Prints: r.prints}
	}
	return Outcome{Status: StatusCompleted, Value: encoded, Prints: r.prints}
}

func (r *run) status(parent context.Context, err error) Status {
	if stop := r.classify(parent, err); stop != nil {
		return stop.status
	}
	return StatusFailed
}

func (r *run) failure(parent context.Context, err error) Failure {
	if stop := r.classify(parent, err); stop != nil {
		return Failure{Code: stop.code, Message: stop.msg, Stage: stageRun}
	}
	if parse, ok := errors.AsType[*parseError](err); ok {
		return Failure{Code: codeSyntax, Message: syntaxMessage(parse.err), Stage: stageParse}
	}
	return Failure{Code: codeError, Message: err.Error(), Stage: stageRun}
}

func (r *run) classify(parent context.Context, err error) *stopError {
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
