package patch

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"path/filepath"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/tool"
)

type Editor struct {
	style Style
}

func New(style string) tool.Editor {
	return Editor{style: canonicalStyle(style)}
}

func (e Editor) Spec() tool.Spec { return specFor(e.style) }

func (e Editor) Class() tool.Class { return tool.ClassEdit }

func (e Editor) Profile() (tool.Profile, bool) {
	return tool.Profile{Style: string(e.style), Version: contractVersion}, e.style.profile()
}

func (e Editor) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	plan, err := e.Plan(ctx, call, env)
	if err != nil {
		return tool.Result{}, err
	}
	return plan.Apply(ctx)
}

func (e Editor) Plan(ctx context.Context, call tool.Call, env tool.Env) (tool.Plan, error) {
	plan, err := e.plan(ctx, call, env)
	if err != nil {
		return tool.Plan{}, e.refuse(err)
	}
	return plan, nil
}

func (e Editor) plan(ctx context.Context, call tool.Call, env tool.Env) (tool.Plan, error) {
	input, err := payload(e.style, call.Args)
	if err != nil {
		return tool.Plan{}, parseError(e.style, err)
	}
	if len(input) > 8<<20 {
		return tool.Plan{}, reasoned(tool.ReasonResourceLimit, errors.New("patch: input is over 8 MiB; split the change into several calls"))
	}
	edits, err := parseCall(e.style, input)
	if err != nil {
		return tool.Plan{}, err
	}
	if env.Masker != nil {
		if unmaskErr := unmaskEdits(edits, env.Masker.UnmaskStrict); unmaskErr != nil {
			return tool.Plan{}, reasoned(tool.ReasonPermissionDenied, fmt.Errorf("patch: secret placeholder cannot be restored: %w", unmaskErr))
		}
	}
	if len(edits) == 0 || len(edits) > 64 {
		return tool.Plan{}, reasoned(tool.ReasonResourceLimit, errors.New("patch: a payload may hold 1 to 64 edits; split the change into several calls"))
	}
	batch, err := stage(ctx, e.style, edits, env, input)
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

const maxPathText = 16 << 10

func editPaths(edits []edit) []string {
	var paths []string
	for _, item := range edits {
		for _, raw := range []string{item.path, item.to} {
			if raw == "" {
				continue
			}
			canonical := filepath.ToSlash(filepath.Clean(raw))
			if !slices.Contains(paths, canonical) {
				paths = append(paths, canonical)
			}
		}
	}
	return paths
}

func checkPathText(paths []string) error {
	total := 0
	for _, path := range paths {
		total += len(path)
	}
	if total > maxPathText {
		return reasoned(tool.ReasonResourceLimit, errors.New("patch: the paths of this call total over 16 KiB; split the change into several calls"))
	}
	return nil
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

func parseCall(style Style, input string) ([]edit, error) {
	edits, err := parse(style, input)
	if err != nil {
		return nil, parseError(style, err)
	}
	if style.profile() {
		if err = checkPathText(editPaths(edits)); err != nil {
			return nil, err
		}
	}
	return edits, nil
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
