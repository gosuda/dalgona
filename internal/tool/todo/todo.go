package todo

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strings"

	"github.com/gosuda/dalgona/internal/tool"
)

type Tool struct{}

type item struct {
	text   string
	status string
}

func New() tool.Tool { return Tool{} }

func (Tool) Spec() tool.Spec {
	return tool.Spec{
		Name:        "todo",
		Description: "Replace the thread plan with items in their execution order. Each item has non-empty text and status pending, in_progress, or done.",
		Schema:      jsontext.Value(`{"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"text":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","done"]}},"required":["text","status"],"additionalProperties":false}}},"required":["items"],"additionalProperties":false}`),
		Keep:        tool.KeepHead,
	}
}

func (Tool) Class() tool.Class { return tool.ClassRead }

func (Tool) Run(_ context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	items, err := parseItems(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	meta := encodeItems(items)
	return tool.Result{Output: meta, OK: true, Meta: meta}, nil
}

func parseItems(raw jsontext.Value) ([]item, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, fmt.Errorf("todo: read arguments: %w", err)
	}
	if start.Kind() != '{' {
		return nil, errors.New("todo: arguments must be an object")
	}
	items, found, err := readItems(decoder)
	if err != nil {
		return nil, err
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, fmt.Errorf("todo: read argument end: %w", err)
	}
	if !found {
		return nil, errors.New("todo: items is required")
	}
	return items, nil
}

func readItems(decoder *jsontext.Decoder) ([]item, bool, error) {
	var items []item
	found := false
	for decoder.PeekKind() != '}' {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, false, fmt.Errorf("todo: read argument name: %w", err)
		}
		nameText := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, false, fmt.Errorf("todo: read argument %s: %w", nameText, err)
		}
		if nameText != "items" {
			continue
		}
		items, err = parseItemArray(value)
		if err != nil {
			return nil, false, err
		}
		found = true
	}
	return items, found, nil
}

func parseItemArray(raw jsontext.Value) ([]item, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, fmt.Errorf("todo: read items: %w", err)
	}
	if start.Kind() != '[' {
		return nil, errors.New("todo: items must be an array")
	}
	items := make([]item, 0, 8)
	for decoder.PeekKind() != ']' {
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, fmt.Errorf("todo: read item %d: %w", len(items)+1, err)
		}
		parsed, err := parseItem(value, len(items)+1)
		if err != nil {
			return nil, err
		}
		items = append(items, parsed)
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, fmt.Errorf("todo: read items end: %w", err)
	}
	return items, nil
}

func parseItem(raw jsontext.Value, index int) (item, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return item{}, fmt.Errorf("todo: read item %d: %w", index, err)
	}
	if start.Kind() != '{' {
		return item{}, fmt.Errorf("todo: item %d must be an object", index)
	}
	var result item
	for decoder.PeekKind() != '}' {
		name, err := decoder.ReadToken()
		if err != nil {
			return item{}, fmt.Errorf("todo: read item %d field: %w", index, err)
		}
		nameText := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return item{}, fmt.Errorf("todo: read item %d value: %w", index, err)
		}
		if err := setItemField(&result, nameText, value); err != nil {
			return item{}, fmt.Errorf("todo: item %d: %w", index, err)
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return item{}, fmt.Errorf("todo: read item %d end: %w", index, err)
	}
	return normalizeItem(result, index)
}

func setItemField(result *item, name string, value jsontext.Value) error {
	if name != "text" && name != "status" {
		return nil
	}
	text, err := decodeString(value)
	if err != nil {
		return fmt.Errorf("%s must be a string: %w", name, err)
	}
	if name == "text" {
		result.text = text
	} else {
		result.status = text
	}
	return nil
}

func decodeString(raw jsontext.Value) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("read string: %w", err)
	}
	if token.Kind() != '"' {
		return "", errors.New("expected string")
	}
	return token.String(), nil
}

func normalizeItem(value item, index int) (item, error) {
	value.text = strings.TrimSpace(value.text)
	if value.text == "" {
		return item{}, fmt.Errorf("todo: item %d text must not be empty", index)
	}
	switch value.status {
	case "pending", "in_progress", "done":
		return value, nil
	default:
		return item{}, fmt.Errorf("todo: item %d status must be one of pending, in_progress, done", index)
	}
}

func encodeItems(items []item) jsontext.Value {
	out := []byte(`{"items":[`)
	for index, value := range items {
		if index > 0 {
			out = append(out, ',')
		}
		out = append(out, `{"text":`...)
		out, _ = jsontext.AppendQuote(out, value.text)
		out = append(out, `,"status":`...)
		out, _ = jsontext.AppendQuote(out, value.status)
		out = append(out, '}')
	}
	return append(out, `]}`...)
}

var _ tool.Tool = Tool{}
