package script

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"math"
	"sync/atomic"
	"time"

	"go.starlark.net/starlark"
	"modernc.org/quickjs"
)

const (
	jsStackSlots = 5000
	jsMemLimit   = 256 << 20
)

var jsEvalBudget = 30 * time.Second

const jsPreamble = `
if (__stopping()) { throw new Error("cancelled"); }
const data = JSON.parse(%s);
const __j = (v) => JSON.stringify(v === undefined ? null : v);
const tools = new Proxy({}, { get: (_t, n) => (a) => JSON.parse(__toolCall(String(n), __j(a === undefined ? {} : a))) });
const __Date = Date;
Date = ((D) => {
  function F(...a) {
    if (new.target === undefined) { return new D(Math.round(__ctxNow() * 1000)).toString(); }
    return a.length ? new D(...a) : new D(Math.round(__ctxNow() * 1000));
  }
  F.now = () => Math.round(__ctxNow() * 1000);
  F.parse = D.parse; F.UTC = D.UTC; F.prototype = D.prototype;
  return F;
})(__Date);
Math.random = (() => { let s = %d; return () => { s = s + 0x6D2B79F5 | 0; let t = Math.imul(s ^ s >>> 15, 1 | s); t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t; return ((t ^ t >>> 14) >>> 0) / 4294967296; }; })();
const ctx = {
  tools: tools,
  log: (m) => { __ctxLog(String(m)); },
  sleep: (s) => { __ctxSleep(Number(s)); },
  spawn: (p, o) => __ctxSpawn(String(p), __j(o === undefined ? {} : o)),
  wait: (ids) => JSON.parse(__ctxWait(__j(ids))),
  judge: (s, q, m) => JSON.parse(__ctxJudge(__j(s), __j(q), String(m === undefined ? "" : m))),
  judgeBatch: (ss, q, m) => JSON.parse(__ctxJudgeBatch(__j(ss), __j(q), String(m === undefined ? "" : m))),
  tryCall: (fn, a) => {
    try { return { ok: true, value: fn(a), error: null }; }
    catch (e) {
      if (__stopping()) { throw e; }
      return { ok: false, value: null, error: String(e && e.message || e) };
    }
  },
};
const console = { log: (...a) => __print(a.map(String).join(" ")) };
const __r = eval(%s);
(() => { try { return __j(__r) } catch (e) { return "null" } })()
`

func (r *Runtime) executeJS(parent context.Context, src, data jsontext.Value, seed int64) Outcome {
	ctx, cancel := context.WithCancelCause(parent)
	r.cancel = cancel
	defer func() {
		r.inflight.Wait()
		cancel(nil)
	}()
	vm, err := quickjs.NewVM()
	if err != nil {
		return stopped(StatusFailed, codeError, "js cell: "+err.Error(), stageRun)
	}
	defer vm.Close()
	vm.SetMaxStackSize(jsStackSlots)
	vm.SetMemoryLimit(jsMemLimit)
	hostCtx, hostCancel := context.WithCancel(ctx)
	defer hostCancel()
	var budgetHit atomic.Bool
	timer := time.AfterFunc(jsEvalBudget, func() {
		budgetHit.Store(true)
		vm.Interrupt()
		hostCancel()
	})
	defer timer.Stop()
	stop := context.AfterFunc(ctx, vm.Interrupt)
	defer stop()
	if bindErr := r.bindJS(vm, hostCtx); bindErr != nil {
		return stopped(StatusFailed, codeError, "js cell: "+bindErr.Error(), stageRun)
	}
	if _, compileErr := vm.Compile(string(src), 0); compileErr != nil {
		return r.finishJS(parent, nil, &parseError{err: compileErr}, false)
	}
	dataLit := "null"
	if len(data) != 0 && data.Kind() != jsontext.KindNull {
		dataLit = string(data)
	}
	out, err := vm.Eval(fmt.Sprintf(jsPreamble, quoteJS(dataLit), seed&0x7FFFFFFF, quoteJS(string(src))), quickjs.EvalGlobal)
	return r.finishJS(parent, jsResult(out), err, budgetHit.Load())
}

func jsResult(out any) jsontext.Value {
	if text, ok := out.(string); ok && jsontext.Value(text).Kind() != jsontext.KindInvalid {
		return jsontext.Value(text)
	}
	return jsontext.Value("null")
}

func quoteJS(src string) string {
	quoted, _ := jsontext.AppendQuote(nil, src)
	return string(quoted)
}

func (r *Runtime) finishJS(parent context.Context, value jsontext.Value, err error, budget bool) Outcome {
	r.inflight.Wait()
	if budget && err != nil && r.fatalStop() == nil && parent.Err() == nil {
		return Outcome{Status: StatusLimitExceeded, Error: Failure{Code: codeLimit, Message: "js cell exceeded the eval time budget", Stage: stageRun}, Prints: r.prints}
	}
	if err != nil || r.fatalStop() != nil {
		return Outcome{Status: r.status(parent, err), Error: r.failure(parent, err), Prints: r.prints}
	}
	if value.Kind() == jsontext.KindInvalid {
		return stopped(StatusFailed, codeResult, "js cell result is not JSON", stageResult)
	}
	return Outcome{Status: StatusCompleted, Value: value, Prints: r.prints}
}

func (r *Runtime) bindJS(vm *quickjs.VM, ctx context.Context) error {
	funcs := map[string]quickjs.HostFunc{
		"__print":    func(args []any) (any, error) { r.print(nil, fmt.Sprint(args...)); return nil, nil },
		"__toolCall": func(args []any) (any, error) { return r.jsToolCall(ctx, args) },
		"__ctxLog": func(args []any) (any, error) {
			r.fx.Log(r.id, fmt.Sprint(args...))
			return nil, nil
		},
		"__ctxSleep":      func(args []any) (any, error) { return nil, r.jsSleep(ctx, args) },
		"__ctxNow":        func(args []any) (any, error) { return r.now(ctx), nil },
		"__ctxSpawn":      func(args []any) (any, error) { return r.jsSpawn(ctx, args) },
		"__ctxWait":       func(args []any) (any, error) { return r.jsWait(ctx, args) },
		"__ctxJudge":      func(args []any) (any, error) { return r.jsJudge(ctx, args) },
		"__ctxJudgeBatch": func(args []any) (any, error) { return r.jsJudgeBatch(ctx, args) },
		"__stopping": func(_ []any) (any, error) {
			return r.fatalStop() != nil || ctx.Err() != nil, nil
		},
	}
	for name, fn := range funcs {
		if err := vm.RegisterHostFunc(name, fn); err != nil {
			return fmt.Errorf("register %s: %w", name, err)
		}
	}
	return nil
}

func jsArg(args []any, i int, fallback string) jsontext.Value {
	if i >= len(args) {
		return jsontext.Value(fallback)
	}
	text, ok := args[i].(string)
	if !ok {
		return jsontext.Value(fallback)
	}
	return jsontext.Value(text)
}

func jsStr(args []any, i int) string {
	if i >= len(args) {
		return ""
	}
	text, _ := args[i].(string)
	return text
}

func jsKwargs(payload jsontext.Value) ([]starlark.Tuple, error) {
	decoded, err := FromJSON(payload)
	if err != nil {
		return nil, err
	}
	if decoded == starlark.None {
		return nil, nil
	}
	dict, ok := decoded.(*starlark.Dict)
	if !ok {
		return nil, errors.New("tool arguments must be a JSON object")
	}
	return dict.Items(), nil
}

func (r *Runtime) jsToolCall(ctx context.Context, args []any) (any, error) {
	if len(args) == 0 {
		return nil, errors.New("tool call needs a name")
	}
	name, _ := args[0].(string)
	payload := jsArg(args, 1, "{}")
	kwargs, err := jsKwargs(payload)
	if err != nil {
		return nil, err
	}
	slot, effect, err := r.prepareTool(name, kwargs)
	if err != nil {
		return nil, err
	}
	value, err := r.resolve(ctx, slot, effect)
	if err != nil {
		return nil, err
	}
	return string(value), nil
}

func (r *Runtime) jsSleep(ctx context.Context, args []any) error {
	seconds, ok := args[0].(float64)
	if !ok || seconds < 0 || math.IsNaN(seconds) || math.IsInf(seconds, 0) {
		return errors.New("ctx.sleep: seconds must be a non-negative finite number")
	}
	return r.sleepFor(ctx, time.Duration(seconds*float64(time.Second)))
}

func (r *Runtime) jsSpawn(ctx context.Context, args []any) (any, error) {
	prompt := jsStr(args, 0)
	var raw struct {
		Runner string `json:"runner"`
		Model  string `json:"model"`
	}
	if err := json.Unmarshal(jsArg(args, 1, "{}"), &raw); err != nil {
		return nil, fmt.Errorf("ctx.spawn: %w", err)
	}
	opts := SpawnOpts{Runner: raw.Runner, Model: raw.Model}
	payload := stringFieldsJSON("prompt", prompt, "runner", opts.Runner, "model", opts.Model)
	slot, err := r.submit(OpSpawn, payload)
	if err != nil {
		return nil, err
	}
	value, err := r.resolve(ctx, slot, func(ctx context.Context) (jsontext.Value, error) {
		id, spawnErr := r.fx.Spawn(ctx, slot, prompt, opts)
		if spawnErr != nil {
			return nil, spawnErr
		}
		return StringJSON(id), nil
	})
	if err != nil {
		return nil, err
	}
	var id string
	if err := json.Unmarshal(value, &id); err != nil {
		return nil, fmt.Errorf("ctx.spawn: child id: %w", err)
	}
	return id, nil
}

func (r *Runtime) jsWait(ctx context.Context, args []any) (any, error) {
	var ids []string
	if err := json.Unmarshal(jsArg(args, 0, "[]"), &ids); err != nil {
		return nil, fmt.Errorf("ctx.wait: ids must be a string array: %w", err)
	}
	texts, err := r.fx.Wait(ctx, ids)
	if err != nil {
		return nil, r.effectError(ctx, err)
	}
	out, err := json.Marshal(texts)
	if err != nil {
		return nil, err
	}
	return string(out), nil
}

func (r *Runtime) jsQuestionSet(raw jsontext.Value) (questionSet, error) {
	value, err := FromJSON(raw)
	if err != nil {
		return questionSet{}, err
	}
	dict, ok := value.(*starlark.Dict)
	if !ok {
		return questionSet{}, errors.New("questions must be an object")
	}
	return parseQuestions(dict)
}

func (r *Runtime) jsJudgeOne(ctx context.Context, state starlark.Value, set questionSet, model string) (jsontext.Value, error) {
	encoded, err := judgeState(state)
	if err != nil {
		return nil, err
	}
	slot, effect, err := r.prepareJudge(encoded, set, model)
	if err != nil {
		return nil, err
	}
	value, err := r.resolve(ctx, slot, effect)
	if err != nil {
		return nil, err
	}
	judged, err := judgeValue(value, set.items)
	if err != nil {
		return nil, err
	}
	return ToJSON(judged)
}

func (r *Runtime) jsJudge(ctx context.Context, args []any) (any, error) {
	set, err := r.jsQuestionSet(jsArg(args, 1, "{}"))
	if err != nil {
		return nil, fmt.Errorf("ctx.judge: %w", err)
	}
	state, err := FromJSON(jsArg(args, 0, "null"))
	if err != nil {
		return nil, fmt.Errorf("ctx.judge: state: %w", err)
	}
	model := jsStr(args, 2)
	out, err := r.jsJudgeOne(ctx, state, set, model)
	if err != nil {
		return nil, fmt.Errorf("ctx.judge: %w", err)
	}
	return string(out), nil
}

func (r *Runtime) jsJudgeBatch(ctx context.Context, args []any) (any, error) {
	set, err := r.jsQuestionSet(jsArg(args, 1, "{}"))
	if err != nil {
		return nil, fmt.Errorf("ctx.judge_batch: %w", err)
	}
	states, err := FromJSON(jsArg(args, 0, "[]"))
	if err != nil {
		return nil, fmt.Errorf("ctx.judge_batch: states: %w", err)
	}
	iterable, ok := states.(starlark.Iterable)
	if !ok {
		return nil, errors.New("ctx.judge_batch: states must be an array")
	}
	items, err := boundedItems(iterable)
	if err != nil {
		return nil, fmt.Errorf("ctx.judge_batch: %w", err)
	}
	model := jsStr(args, 2)
	out := []byte{'['}
	for i, item := range items {
		if i > 0 {
			out = append(out, ',')
		}
		entry, err := r.jsBatchEntry(ctx, item, set, model)
		if err != nil {
			return nil, err
		}
		out = append(out, entry...)
	}
	out = append(out, ']')
	return string(out), nil
}

func (r *Runtime) jsBatchEntry(ctx context.Context, item starlark.Value, set questionSet, model string) (jsontext.Value, error) {
	value, err := r.jsJudgeOne(ctx, item, set, model)
	if err == nil {
		return mergeErrorField(value), nil
	}
	if r.fatalStop() != nil || ctx.Err() != nil {
		return nil, err
	}
	out := []byte(`{"answers":null,"model":null,"fallback":null,"error":`)
	out, _ = jsontext.AppendQuote(out, err.Error())
	return jsontext.Value(append(out, '}')), nil
}

func mergeErrorField(value jsontext.Value) jsontext.Value {
	if len(value) == 0 || value[0] != '{' {
		return jsontext.Value(`{"answers":null,"model":null,"fallback":null,"error":"judgment is not an object"}`)
	}
	out := make([]byte, 0, len(value)+14)
	out = append(out, value[:len(value)-1]...)
	out = append(out, `,"error":null}`...)
	return jsontext.Value(out)
}
