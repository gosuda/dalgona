package script

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"maps"
	"math"
	"slices"
	"strconv"
	"time"

	"go.starlark.net/starlark"
	"go.starlark.net/starlarkstruct"

	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const (
	maxScopeLimit = 64
	maxPending    = 4096
)

type effectFunc func(context.Context) (jsontext.Value, error)

type optString string

func (s *optString) Unpack(value starlark.Value) error {
	switch value := value.(type) {
	case starlark.NoneType:
		*s = ""
		return nil
	case starlark.String:
		*s = optString(value)
		return nil
	default:
		return fmt.Errorf("got %s, want string or None", value.Type())
	}
}

func (r *run) ctxValue() starlark.Value {
	return starlarkstruct.FromStringDict(starlark.String("ctx"), starlark.StringDict{
		"tools":       &toolsValue{r: r},
		"try_call":    starlark.NewBuiltin("ctx.try_call", r.tryCall),
		"scope":       starlark.NewBuiltin("ctx.scope", r.scope),
		"spawn":       starlark.NewBuiltin("ctx.spawn", r.spawn),
		"wait":        starlark.NewBuiltin("ctx.wait", r.wait),
		"sleep":       starlark.NewBuiltin("ctx.sleep", r.sleep),
		"log":         starlark.NewBuiltin("ctx.log", r.log),
		"judge":       starlark.NewBuiltin("ctx.judge", r.judge),
		"judge_batch": starlark.NewBuiltin("ctx.judge_batch", r.judgeBatch),
	})
}

func (r *run) effectError(ctx context.Context, err error) error {
	if stop, ok := errors.AsType[*stopError](err); ok {
		return r.halt(stop)
	}
	if errors.Is(err, ErrDenied) {
		return r.halt(&stopError{status: StatusDenied, code: codeDenied, msg: err.Error()})
	}
	if ctx.Err() != nil {
		return r.halt(r.cancelled(ctx))
	}
	return err
}

type toolsValue struct {
	r     *run
	scope *scopeValue
}

func (t *toolsValue) String() string        { return "tools" }
func (t *toolsValue) Type() string          { return "tools" }
func (t *toolsValue) Freeze()               {}
func (t *toolsValue) Truth() starlark.Bool  { return starlark.True }
func (t *toolsValue) Hash() (uint32, error) { return 0, errors.New("unhashable type: tools") }

func (t *toolsValue) Attr(name string) (starlark.Value, error) {
	if !t.r.cfg.reach.has(name) {
		return nil, fmt.Errorf("tool %s is not reachable from this script", name)
	}
	return t.bind(name), nil
}

func (t *toolsValue) AttrNames() []string {
	return slices.Sorted(maps.Keys(t.r.cfg.reach))
}

func (t *toolsValue) Get(key starlark.Value) (starlark.Value, bool, error) {
	name, ok := starlark.AsString(key)
	if !ok {
		return nil, false, fmt.Errorf("tools key must be a string, got %s", key.Type())
	}
	value, err := t.Attr(name)
	return value, err == nil, err
}

func (t *toolsValue) bind(name string) *starlark.Builtin {
	return starlark.NewBuiltin("tools."+name, func(thread *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		if len(args) > 0 {
			return nil, fmt.Errorf("%s takes keyword arguments only", b.Name())
		}
		slot, effect, err := t.r.prepareTool(name, kwargs)
		if err != nil {
			return nil, err
		}
		ctx := runContext(thread)
		if t.scope != nil {
			return t.scope.submit(ctx, slot, effect)
		}
		value, err := t.r.resolve(ctx, slot, effect)
		if err != nil {
			return nil, err
		}
		return fromJSON(value)
	})
}

func (r *run) prepareTool(name string, kwargs []starlark.Tuple) (Slot, effectFunc, error) {
	if name == asktool.Name {
		return Slot{}, nil, r.halt(&stopError{status: StatusDenied, code: codeScope, msg: "tool ask needs the user and runs only as a direct call"})
	}
	if !r.cfg.allow.has(name) {
		return Slot{}, nil, r.halt(&stopError{status: StatusDenied, code: codeScope, msg: fmt.Sprintf("tool %s is not in this script's uses", name)})
	}
	payload, err := kwargsJSON(kwargs)
	if err != nil {
		return Slot{}, nil, fmt.Errorf("tools.%s: %w", name, err)
	}
	slot, err := r.submit(ToolOp(name), payload)
	if err != nil {
		return Slot{}, nil, err
	}
	effect := func(ctx context.Context) (jsontext.Value, error) {
		return r.fx.CallTool(ctx, slot, name, payload)
	}
	return slot, effect, nil
}

type future struct {
	done  chan struct{}
	value jsontext.Value
	err   error
}

func (r *run) async(ctx context.Context, sem chan struct{}, slot Slot, effect effectFunc) *future {
	f := &future{done: make(chan struct{})}
	r.inflight.Go(func() {
		defer close(f.done)
		select {
		case sem <- struct{}{}:
		case <-ctx.Done():
			f.err = r.halt(r.cancelled(ctx))
			return
		}
		defer func() { <-sem }()
		f.value, f.err = r.resolve(ctx, slot, effect)
	})
	return f
}

func settle(pending []*future) ([]starlark.Value, error) {
	for _, f := range pending {
		<-f.done
	}
	values := make([]starlark.Value, len(pending))
	for i, f := range pending {
		if f.err != nil {
			return nil, f.err
		}
		value, err := fromJSON(f.value)
		if err != nil {
			return nil, err
		}
		values[i] = value
	}
	return values, nil
}

type scopeValue struct {
	r       *run
	sem     chan struct{}
	pending []*future
}

func (s *scopeValue) String() string        { return fmt.Sprintf("scope(limit = %d)", cap(s.sem)) }
func (s *scopeValue) Type() string          { return "scope" }
func (s *scopeValue) Freeze()               {}
func (s *scopeValue) Truth() starlark.Bool  { return starlark.True }
func (s *scopeValue) Hash() (uint32, error) { return 0, errors.New("unhashable type: scope") }
func (s *scopeValue) AttrNames() []string   { return []string{"all", "tools"} }

func (s *scopeValue) submit(ctx context.Context, slot Slot, effect effectFunc) (starlark.Value, error) {
	if len(s.pending) >= maxPending {
		return nil, s.r.halt(fmt.Errorf("scope has %d pending calls; call all() before submitting more", maxPending))
	}
	s.pending = append(s.pending, s.r.async(ctx, s.sem, slot, effect))
	return starlark.None, nil
}

func (s *scopeValue) Attr(name string) (starlark.Value, error) {
	switch name {
	case "tools":
		return &toolsValue{r: s.r, scope: s}, nil
	case "all":
		return starlark.NewBuiltin("scope.all", s.all), nil
	default:
		return nil, nil
	}
}

func (s *scopeValue) all(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 0); err != nil {
		return nil, err
	}
	pending := s.pending
	s.pending = nil
	values, err := settle(pending)
	if err != nil {
		return nil, err
	}
	return starlark.NewList(values), nil
}

func scopeLimit(limit int) (chan struct{}, error) {
	if limit < 1 || limit > maxScopeLimit {
		return nil, fmt.Errorf("limit must be between 1 and %d, got %d", maxScopeLimit, limit)
	}
	return make(chan struct{}, limit), nil
}

func (r *run) scope(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var limit int
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "limit", &limit); err != nil {
		return nil, err
	}
	sem, err := scopeLimit(limit)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", b.Name(), err)
	}
	return &scopeValue{r: r, sem: sem}, nil
}

func (r *run) tryCall(thread *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	if len(args) == 0 {
		return nil, fmt.Errorf("%s: missing op", b.Name())
	}
	value, err := starlark.Call(thread, args[0], args[1:], kwargs)
	if err == nil {
		return callResult(true, value, starlark.None), nil
	}
	if r.fatalStop() != nil || runContext(thread).Err() != nil {
		return nil, err
	}
	if _, ok := errors.AsType[*stopError](err); ok {
		return nil, err
	}
	return callResult(false, starlark.None, starlark.String(err.Error())), nil
}

func callResult(ok bool, value, failure starlark.Value) starlark.Value {
	return starlarkstruct.FromStringDict(starlarkstruct.Default, starlark.StringDict{
		"ok":    starlark.Bool(ok),
		"value": value,
		"error": failure,
	})
}

func (r *run) spawn(thread *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var prompt string
	var runnerName, modelName optString
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "prompt", &prompt, "runner?", &runnerName, "model?", &modelName); err != nil {
		return nil, err
	}
	opts := SpawnOpts{Runner: string(runnerName), Model: string(modelName)}
	payload := stringFieldsJSON("prompt", prompt, "runner", opts.Runner, "model", opts.Model)
	slot, err := r.submit(OpSpawn, payload)
	if err != nil {
		return nil, err
	}
	value, err := r.resolve(runContext(thread), slot, func(ctx context.Context) (jsontext.Value, error) {
		id, spawnErr := r.fx.Spawn(ctx, slot, prompt, opts)
		if spawnErr != nil {
			return nil, spawnErr
		}
		return stringJSON(id), nil
	})
	if err != nil {
		return nil, err
	}
	return fromJSON(value)
}

func (r *run) wait(thread *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var ids starlark.Iterable
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "ids", &ids); err != nil {
		return nil, err
	}
	names, err := stringList(ids)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", b.Name(), err)
	}
	ctx := runContext(thread)
	texts, err := r.fx.Wait(ctx, names)
	if err != nil {
		return nil, r.effectError(ctx, err)
	}
	values := make([]starlark.Value, len(texts))
	for i, text := range texts {
		values[i] = starlark.String(text)
	}
	return starlark.NewList(values), nil
}

func stringList(items starlark.Iterable) ([]string, error) {
	iter := items.Iterate()
	defer iter.Done()
	var names []string
	var item starlark.Value
	for iter.Next(&item) {
		name, ok := starlark.AsString(item)
		if !ok {
			return nil, fmt.Errorf("want a list of strings, got an element of type %s", item.Type())
		}
		names = append(names, name)
	}
	return names, nil
}

func (r *run) sleep(thread *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var seconds float64
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "seconds", &seconds); err != nil {
		return nil, err
	}
	if seconds < 0 || math.IsNaN(seconds) || math.IsInf(seconds, 0) {
		return nil, fmt.Errorf("%s: seconds must be non-negative and finite", b.Name())
	}
	if err := r.sleepFor(runContext(thread), time.Duration(seconds*float64(time.Second))); err != nil {
		return nil, err
	}
	return starlark.None, nil
}

func (r *run) sleepFor(ctx context.Context, d time.Duration) error {
	slot, err := r.submit(OpSleep, strconv.AppendInt(nil, int64(d), 10))
	if err != nil {
		return err
	}
	value, err := r.resolve(ctx, slot, r.recordLive(slot, func() jsontext.Value {
		return strconv.AppendInt(nil, time.Now().Add(d).UnixNano(), 10)
	}))
	if err != nil {
		return err
	}
	deadline, err := strconv.ParseInt(string(value), 10, 64)
	if err != nil {
		return r.halt(fmt.Errorf("effect %d: recorded sleep deadline %s is not an integer", slot.N, value))
	}
	timer := time.NewTimer(time.Until(time.Unix(0, deadline)))
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return r.halt(r.cancelled(ctx))
	}
}

func (r *run) now(ctx context.Context) float64 {
	slot, err := r.submit(OpTime, jsontext.Value("null"))
	if err != nil {
		return 0
	}
	value, err := r.resolve(ctx, slot, r.recordLive(slot, func() jsontext.Value {
		now := time.Now()
		seconds := float64(now.Unix()) + float64(now.Nanosecond())/1e9
		return strconv.AppendFloat(nil, seconds, 'g', -1, 64)
	}))
	if err != nil {
		_ = r.halt(err)
		return 0
	}
	seconds, err := strconv.ParseFloat(string(value), 64)
	if err != nil {
		_ = r.halt(fmt.Errorf("effect %d: recorded time %s is not a number", slot.N, value))
	}
	return seconds
}

func (r *run) recordLive(slot Slot, produce func() jsontext.Value) effectFunc {
	return func(context.Context) (jsontext.Value, error) {
		value := produce()
		if err := r.record(slot, value); err != nil {
			return nil, err
		}
		return value, nil
	}
}

func (r *run) log(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var msg string
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "msg", &msg); err != nil {
		return nil, err
	}
	r.fx.Log(r.id, msg)
	return starlark.None, nil
}

func stringFieldsJSON(pairs ...string) jsontext.Value {
	out := []byte{'{'}
	for i := 0; i+1 < len(pairs); i += 2 {
		if i > 0 {
			out = append(out, ',')
		}
		out, _ = jsontext.AppendQuote(out, pairs[i])
		out = append(out, ':')
		out, _ = jsontext.AppendQuote(out, pairs[i+1])
	}
	return append(out, '}')
}
