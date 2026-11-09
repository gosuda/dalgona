package script

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"

	"go.starlark.net/starlark"
	"go.starlark.net/starlarkstruct"
)

const defaultJudgeConcurrency = 8

type question struct {
	name string
	kind string
}

type questionSet struct {
	raw   jsontext.Value
	items []question
}

var answerFields = map[string][]string{
	"choice": {"choice", "probabilities", "confidence"},
	"noul":   {"noul"},
	"score":  {"score", "probabilities", "confidence"},
}

func (r *run) judge(thread *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var state starlark.Value
	var questions *starlark.Dict
	var model optString
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "state", &state, "questions", &questions, "model?", &model); err != nil {
		return nil, err
	}
	set, err := parseQuestions(questions)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", b.Name(), err)
	}
	encoded, err := judgeState(state)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", b.Name(), err)
	}
	slot, effect, err := r.prepareJudge(encoded, set, string(model))
	if err != nil {
		return nil, err
	}
	value, err := r.resolve(runContext(thread), slot, effect)
	if err != nil {
		return nil, err
	}
	return answersValue(value, set.items)
}

func (r *run) judgeBatch(thread *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var states starlark.Iterable
	var questions *starlark.Dict
	var model optString
	concurrency := defaultJudgeConcurrency
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "states", &states, "questions", &questions, "concurrency?", &concurrency, "model?", &model); err != nil {
		return nil, err
	}
	set, err := parseQuestions(questions)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", b.Name(), err)
	}
	sem, err := scopeLimit(concurrency)
	if err != nil {
		return nil, fmt.Errorf("%s: concurrency %w", b.Name(), err)
	}
	items, err := boundedItems(states)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", b.Name(), err)
	}
	futures, err := r.submitJudges(runContext(thread), sem, items, set, string(model))
	if err != nil {
		return nil, err
	}
	return r.batchResults(futures, set.items)
}

func boundedItems(states starlark.Iterable) ([]starlark.Value, error) {
	iter := states.Iterate()
	defer iter.Done()
	var items []starlark.Value
	var item starlark.Value
	for iter.Next(&item) {
		if len(items) == maxPending {
			return nil, fmt.Errorf("at most %d states per batch", maxPending)
		}
		items = append(items, item)
	}
	return items, nil
}

func (r *run) submitJudges(ctx context.Context, sem chan struct{}, items []starlark.Value, set questionSet, model string) ([]*future, error) {
	futures := make([]*future, len(items))
	for i, item := range items {
		encoded, err := judgeState(item)
		if err != nil {
			futures[i] = failedFuture(err)
			continue
		}
		slot, effect, err := r.prepareJudge(encoded, set, model)
		if err != nil {
			return nil, err
		}
		futures[i] = r.async(ctx, sem, slot, effect)
	}
	return futures, nil
}

func failedFuture(err error) *future {
	f := &future{done: make(chan struct{}), err: err}
	close(f.done)
	return f
}

func (r *run) batchResults(futures []*future, items []question) (starlark.Value, error) {
	for _, f := range futures {
		<-f.done
	}
	if stop := r.fatalStop(); stop != nil {
		return nil, stop
	}
	results := make([]starlark.Value, len(futures))
	for i, f := range futures {
		results[i] = batchItem(f, items)
	}
	return starlark.NewList(results), nil
}

func batchItem(f *future, items []question) starlark.Value {
	err := f.err
	var answers starlark.Value = starlark.None
	if err == nil {
		answers, err = answersValue(f.value, items)
	}
	if err != nil {
		return starlarkstruct.FromStringDict(starlarkstruct.Default, starlark.StringDict{"answers": starlark.None, "error": starlark.String(err.Error())})
	}
	return starlarkstruct.FromStringDict(starlarkstruct.Default, starlark.StringDict{"answers": answers, "error": starlark.None})
}

func (r *run) prepareJudge(state jsontext.Value, set questionSet, model string) (Slot, effectFunc, error) {
	payload := judgePayload(state, set.raw, model)
	slot, err := r.submit(OpJudge, payload)
	if err != nil {
		return Slot{}, nil, err
	}
	effect := func(ctx context.Context) (jsontext.Value, error) {
		value, err := r.fx.Judge(ctx, slot, JudgeRequest{State: state, Questions: set.raw, Model: model})
		if err != nil {
			return nil, err
		}
		if recErr := r.record(slot, value); recErr != nil {
			return nil, recErr
		}
		return value, nil
	}
	return slot, effect, nil
}

func judgePayload(state, questions jsontext.Value, model string) jsontext.Value {
	out := make([]byte, 0, len(state)+len(questions)+len(model)+40)
	out = append(out, `{"state":`...)
	out = append(out, state...)
	out = append(out, `,"questions":`...)
	out = append(out, questions...)
	out = append(out, `,"model":`...)
	out, _ = jsontext.AppendQuote(out, model)
	return append(out, '}')
}

func judgeState(state starlark.Value) (jsontext.Value, error) {
	encoded, err := toJSON(state)
	if err != nil {
		return nil, fmt.Errorf("state: %w", err)
	}
	switch encoded.Kind() {
	case jsontext.KindString, jsontext.KindBeginObject, jsontext.KindBeginArray:
		return encoded, nil
	case jsontext.KindInvalid, jsontext.KindNull, jsontext.KindFalse, jsontext.KindTrue, jsontext.KindNumber, jsontext.KindEndObject, jsontext.KindEndArray:
		return nil, errors.New("state must be a string, dict, or list")
	}
	return nil, errors.New("state must be a string, dict, or list")
}

func parseQuestions(questions *starlark.Dict) (questionSet, error) {
	if questions.Len() == 0 {
		return questionSet{}, errors.New("questions must not be empty")
	}
	var buf bytes.Buffer
	enc := newEncoder(&buf)
	set := questionSet{items: make([]question, 0, questions.Len())}
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return questionSet{}, err
	}
	for _, entry := range questions.Items() {
		item, err := writeQuestion(enc, entry[0], entry[1])
		if err != nil {
			return questionSet{}, err
		}
		set.items = append(set.items, item)
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return questionSet{}, err
	}
	set.raw = jsontext.Value(bytes.TrimSuffix(buf.Bytes(), []byte("\n")))
	return set, nil
}

func writeQuestion(enc *jsontext.Encoder, key, value starlark.Value) (question, error) {
	name, ok := key.(starlark.String)
	if !ok {
		return question{}, fmt.Errorf("question name must be a string, got %s", key.Type())
	}
	fields, ok := value.(*starlark.Dict)
	if !ok {
		return question{}, fmt.Errorf("question %s must be a dict, got %s", name, value.Type())
	}
	kind, err := questionKind(string(name), fields)
	if err != nil {
		return question{}, err
	}
	if err := enc.WriteToken(jsontext.String(string(name))); err != nil {
		return question{}, err
	}
	if err := writeQuestionFields(enc, fields, kind); err != nil {
		return question{}, fmt.Errorf("question %s: %w", name, err)
	}
	return question{name: string(name), kind: kind}, nil
}

func questionKind(name string, fields *starlark.Dict) (string, error) {
	value, found, _ := fields.Get(starlark.String("type"))
	kind, ok := starlark.AsString(value)
	if !found || !ok {
		return "", fmt.Errorf("question %s needs a string type", name)
	}
	if kind == "bool" {
		kind = "noul"
	}
	if _, known := answerFields[kind]; !known {
		return "", fmt.Errorf("question %s has unsupported type %q; want choice, noul, bool, or score", name, kind)
	}
	return kind, nil
}

func writeQuestionFields(enc *jsontext.Encoder, fields *starlark.Dict, kind string) error {
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, entry := range fields.Items() {
		key, ok := entry[0].(starlark.String)
		if !ok {
			return fmt.Errorf("field name must be a string, got %s", entry[0].Type())
		}
		value := entry[1]
		if key == "type" {
			value = starlark.String(kind)
		}
		if err := writeMember(enc, string(key), value, 1); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndObject)
}

func answersValue(raw jsontext.Value, items []question) (starlark.Value, error) {
	decoded, err := fromJSON(raw)
	if err != nil {
		return nil, fmt.Errorf("judge answers: %w", err)
	}
	answers, ok := decoded.(*starlark.Dict)
	if !ok {
		return nil, errors.New("judge answers must be a JSON object")
	}
	out := starlark.NewDict(len(items))
	for _, item := range items {
		answer, err := answerStruct(answers, item)
		if err != nil {
			return nil, err
		}
		if err := out.SetKey(starlark.String(item.name), answer); err != nil {
			return nil, err
		}
	}
	return out, nil
}

func answerStruct(answers *starlark.Dict, item question) (starlark.Value, error) {
	value, found, _ := answers.Get(starlark.String(item.name))
	answer, ok := value.(*starlark.Dict)
	if !found || !ok {
		return nil, fmt.Errorf("judge answers miss question %s", item.name)
	}
	fields := answerFields[item.kind]
	members := make(starlark.StringDict, len(fields))
	for _, field := range fields {
		member, present, _ := answer.Get(starlark.String(field))
		if !present {
			member = starlark.None
		}
		members[field] = member
	}
	return starlarkstruct.FromStringDict(starlarkstruct.Default, members), nil
}
