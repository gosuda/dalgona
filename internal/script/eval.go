package script

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strings"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
)

const evalSchema = `{"type":"object","properties":{"code":{"type":"string","description":"Starlark cell. The last expression is the result."},"uses":{"type":"array","items":{"type":"string"},"description":"Tool names this cell may call. Each MUST be in the eval allow list. Omit to use the whole allow list."},"data":{"description":"Any JSON value. The cell reads it as data."}},"required":["code"],"additionalProperties":false}`

const evalDescription = "Run one Starlark cell as a durable script. The cell sees `ctx`, `tools` (the same as `ctx.tools`), and `data` (the request `data`, or None). The last expression is the result. `print` lines are captured. Call a tool with `tools.<name>(arg = value)` or `tools[\"<name>\"](arg = value)`; only tools in the cell's uses run. A failed call stops the cell; `ctx.try_call(tools.<name>, arg = value)` returns `struct(ok, value, error)` instead. Run calls at the same time with `s = ctx.scope(limit)`, `s.tools.<name>(...)`, and `s.all()`, which returns results in submission order. Start children with `ctx.spawn(prompt)` and join them with `ctx.wait(ids)`. Judge with `ctx.judge(state, questions)` and `ctx.judge_batch(states, questions)`. A recovered cell replays recorded effects; a cell whose effect was interrupted ends with status `indeterminate`."

type EvalOptions struct {
	Mode   config.ToolMode
	Tools  []tool.Spec
	Config config.EvalConfig
}

type Eval struct {
	fx    Effects
	spec  tool.Spec
	reach nameSet
	allow nameSet
}

type evalRequest struct {
	code    string
	uses    []string
	usesSet bool
	data    jsontext.Value
}

func NewEval(fx Effects, opts EvalOptions) *Eval {
	tools := make([]tool.Spec, 0, len(opts.Tools))
	names := make([]string, 0, len(opts.Tools))
	for _, spec := range opts.Tools {
		if spec.Name != EvalName {
			tools = append(tools, spec)
			names = append(names, spec.Name)
		}
	}
	allow := opts.Config.Uses
	if !opts.Config.Set {
		allow = DefaultUses(opts.Mode, tools)
	}
	return &Eval{
		fx:    fx,
		spec:  tool.Spec{Name: EvalName, Description: describeEval(FoldedTools(opts.Mode, tools)), Schema: jsontext.Value(evalSchema)},
		reach: newNameSet(names),
		allow: newNameSet(allow),
	}
}

func describeEval(folded []tool.Spec) string {
	if len(folded) == 0 {
		return evalDescription
	}
	var b strings.Builder
	b.WriteString(evalDescription)
	b.WriteString("\n\nTools reachable only from cells:")
	for _, spec := range folded {
		line, _, _ := strings.Cut(spec.Description, "\n")
		b.WriteString("\n")
		b.WriteString(spec.Name)
		b.WriteString(": ")
		b.WriteString(strings.TrimSpace(line))
	}
	return b.String()
}

func (e *Eval) Spec() tool.Spec   { return e.spec }
func (e *Eval) Class() tool.Class { return tool.ClassScript }

func (e *Eval) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	req, _, outcome := e.request(call.Args)
	if outcome.Status != "" {
		return replyResult(call.ID, outcome)
	}
	start, done, err := e.fx.Started(RunStart{ID: call.ID, Entry: EvalName, Source: []byte(req.code), Args: call.Args, Seed: newSeed()})
	if err != nil {
		return tool.Result{}, fmt.Errorf("eval %s: start run: %w", call.ID, err)
	}
	if done != nil {
		return replyResult(call.ID, *done)
	}
	outcome, err = complete(e.fx, start.ID, e.started(ctx, start))
	if err != nil {
		return tool.Result{}, fmt.Errorf("eval %s: %w", call.ID, err)
	}
	return replyResult(call.ID, outcome)
}

func (e *Eval) started(ctx context.Context, start RunStart) Outcome {
	req, allow, outcome := e.request(start.Args)
	if outcome.Status != "" {
		return outcome
	}
	return e.execute(ctx, start, req, allow)
}

func (e *Eval) request(args jsontext.Value) (evalRequest, nameSet, Outcome) {
	req, err := parseEvalArgs(args)
	if err != nil {
		return evalRequest{}, nil, stopped(StatusFailed, codeInvalid, "eval: "+err.Error(), stageArgs)
	}
	if !req.usesSet {
		return req, e.allow, Outcome{}
	}
	for _, name := range req.uses {
		if !e.allow.has(name) {
			return evalRequest{}, nil, stopped(StatusDenied, codeScope, fmt.Sprintf("uses entry %s is not in the eval allow list", name), stageUses)
		}
	}
	return req, newNameSet(req.uses), Outcome{}
}

func (e *Eval) execute(ctx context.Context, start RunStart, req evalRequest, allow nameSet) Outcome {
	r := newRun(e.fx, start.ID, runConfig{reach: e.reach, allow: allow})
	env, end := r.begin(ctx, start.Seed)
	defer end()
	data, err := fromJSON(req.data)
	if err != nil {
		return stopped(StatusFailed, codeInvalid, "eval: data: "+err.Error(), stageArgs)
	}
	env = r.withContext(env)
	env["data"] = data
	value, err := r.execCell(EvalName, start.Source, env)
	return r.finish(ctx, value, err)
}

func parseEvalArgs(args jsontext.Value) (evalRequest, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(args))
	if tok, err := dec.ReadToken(); err != nil || tok.Kind() != jsontext.KindBeginObject {
		return evalRequest{}, errors.New("arguments must be a JSON object")
	}
	var req evalRequest
	codeSet := false
	for dec.PeekKind() == jsontext.KindString {
		key, err := dec.ReadToken()
		if err != nil {
			return evalRequest{}, err
		}
		if err := req.readField(dec, key.String(), &codeSet); err != nil {
			return evalRequest{}, err
		}
	}
	if _, err := dec.ReadToken(); err != nil {
		return evalRequest{}, err
	}
	if !codeSet {
		return evalRequest{}, errors.New("code is required")
	}
	return req, nil
}

func (req *evalRequest) readField(dec *jsontext.Decoder, key string, codeSet *bool) error {
	switch key {
	case "code":
		tok, err := dec.ReadToken()
		if err != nil || tok.Kind() != jsontext.KindString {
			return errors.New("code must be a string")
		}
		req.code, *codeSet = tok.String(), true
		return nil
	case "uses":
		uses, err := readStrings(dec)
		if err != nil {
			return fmt.Errorf("uses: %w", err)
		}
		req.uses, req.usesSet = uses, true
		return nil
	case "data":
		value, err := dec.ReadValue()
		if err != nil {
			return fmt.Errorf("data: %w", err)
		}
		req.data = value.Clone()
		return nil
	default:
		return fmt.Errorf("unknown field %q", key)
	}
}

func readStrings(dec *jsontext.Decoder) ([]string, error) {
	if tok, err := dec.ReadToken(); err != nil || tok.Kind() != jsontext.KindBeginArray {
		return nil, errors.New("must be an array of strings")
	}
	items := []string{}
	for dec.PeekKind() == jsontext.KindString {
		tok, err := dec.ReadToken()
		if err != nil {
			return nil, err
		}
		items = append(items, tok.String())
	}
	if tok, err := dec.ReadToken(); err != nil || tok.Kind() != jsontext.KindEndArray {
		return nil, errors.New("must be an array of strings")
	}
	return items, nil
}

func replyResult(id string, outcome Outcome) (tool.Result, error) {
	reply, err := outcome.Reply(id)
	if err != nil {
		return tool.Result{}, err
	}
	return tool.Result{Output: reply, OK: outcome.Status == StatusCompleted}, nil
}

func (o Outcome) Reply(id string) ([]byte, error) {
	var buf bytes.Buffer
	enc := newEncoder(&buf)
	err := errors.Join(
		enc.WriteToken(jsontext.BeginObject),
		writeString(enc, "invocation_id", id),
		writeString(enc, "status", string(o.Status)),
		o.writeResult(enc),
		o.writePrints(enc),
		enc.WriteToken(jsontext.EndObject),
	)
	if err != nil {
		return nil, fmt.Errorf("encode reply: %w", err)
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

func (o Outcome) writeResult(enc *jsontext.Encoder) error {
	if o.Status == StatusCompleted {
		return errors.Join(enc.WriteToken(jsontext.String("value")), enc.WriteValue(o.Value))
	}
	return errors.Join(
		enc.WriteToken(jsontext.String("error")),
		enc.WriteToken(jsontext.BeginObject),
		writeString(enc, "code", o.Error.Code),
		writeString(enc, "message", o.Error.Message),
		writeString(enc, "stage", o.Error.Stage),
		enc.WriteToken(jsontext.EndObject),
	)
}

func (o Outcome) writePrints(enc *jsontext.Encoder) error {
	err := errors.Join(
		enc.WriteToken(jsontext.String("prints")),
		enc.WriteToken(jsontext.BeginObject),
		enc.WriteToken(jsontext.String("lines")),
		enc.WriteToken(jsontext.BeginArray),
	)
	for _, line := range o.Prints.Lines {
		err = errors.Join(err, enc.WriteToken(jsontext.String(line)))
	}
	return errors.Join(err,
		enc.WriteToken(jsontext.EndArray),
		enc.WriteToken(jsontext.String("truncated")),
		enc.WriteToken(jsontext.Bool(o.Prints.Truncated)),
		enc.WriteToken(jsontext.EndObject),
	)
}

func writeString(enc *jsontext.Encoder, key, value string) error {
	return errors.Join(enc.WriteToken(jsontext.String(key)), enc.WriteToken(jsontext.String(value)))
}
