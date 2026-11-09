package patch

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"

	"github.com/gosuda/dalgona/internal/tool"
)

type Editor struct {
	style Style
}

func New(style string) tool.Editor {
	return Editor{style: canonicalStyle(style)}
}

func (e Editor) Spec() tool.Spec { return specFor(e.style) }

func (e Editor) Class() tool.Class { return tool.ClassEdit }

func (e Editor) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	plan, err := e.Plan(ctx, call, env)
	if err != nil {
		return tool.Result{}, err
	}
	return plan.Apply(ctx)
}

func (e Editor) Plan(ctx context.Context, call tool.Call, env tool.Env) (tool.Plan, error) {
	input, err := payload(e.style, call.Args)
	if err != nil {
		return tool.Plan{}, parseError(e.style, err)
	}
	if len(input) > 8<<20 {
		return tool.Plan{}, errors.New("patch: input is over 8 MiB; split the change into several calls")
	}
	edits, err := parse(e.style, input)
	if err != nil {
		return tool.Plan{}, parseError(e.style, err)
	}
	if env.Masker != nil {
		if unmaskErr := unmaskEdits(edits, env.Masker.UnmaskStrict); unmaskErr != nil {
			return tool.Plan{}, fmt.Errorf("patch: secret placeholder cannot be restored: %w", unmaskErr)
		}
	}
	if len(edits) == 0 || len(edits) > 64 {
		return tool.Plan{}, errors.New("patch: a payload may hold 1 to 64 edits; split the change into several calls")
	}
	batch, err := stage(ctx, e.style, edits, env)
	if err != nil {
		return tool.Plan{}, err
	}
	files := batch.changes()
	return tool.Plan{
		Files: files,
		Apply: func(ctx context.Context) (tool.Result, error) {
			return applyBatch(ctx, env, batch)
		},
		Discard: batch.closePins,
	}, nil
}

func payload(style Style, raw jsontext.Value) (string, error) {
	if style == Replace {
		return string(raw), nil
	}
	if input, err := jsonString(raw); err == nil {
		return input, nil
	}
	object, err := decodeObject(raw)
	if err != nil {
		return "", fmt.Errorf("patch: invalid input: expected a string or an input object: %w", err)
	}
	value, ok := object["input"]
	if !ok || len(object) != 1 {
		return "", errors.New("patch: invalid input: the object must hold only input")
	}
	input, err := jsonString(value)
	if err != nil {
		return "", fmt.Errorf("patch: invalid input: input must be a string: %w", err)
	}
	return input, nil
}

func jsonString(raw jsontext.Value) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewBuffer(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != '"' {
		return "", errors.New("expected string")
	}
	text := token.String()
	if _, tailErr := decoder.ReadToken(); !errors.Is(tailErr, io.EOF) {
		return "", errors.New("trailing data after string")
	}
	return text, nil
}

func parse(style Style, input string) ([]edit, error) {
	switch style {
	case Anchor:
		return parseAnchor(input)
	case Replace:
		return parseReplace(input)
	case Hashline, HashlineLight, HashlineEnhanced:
		return parseHashline(input, style)
	case ApplyPatch:
		return parseApplyPatch(input)
	default:
		return nil, fmt.Errorf("patch: unknown edit style %q", style)
	}
}
