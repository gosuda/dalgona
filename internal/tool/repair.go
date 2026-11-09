package tool

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
)

const maxArgumentRepairs = 16

var errTooManyRepairs = errors.New("too many argument repairs")

type objectEntry struct {
	name  string
	value jsontext.Value
}

func Repair(schema jsontext.Value, raw jsontext.Value) (jsontext.Value, int, error) {
	properties, err := schemaProperties(schema)
	if err != nil {
		return nil, 0, err
	}
	entries, err := readObject(raw)
	if err != nil {
		return nil, 0, err
	}
	kept, repairs, err := repairEntries(properties, entries)
	if err != nil {
		return nil, repairs, err
	}
	result, err := writeObject(kept)
	if err != nil {
		return nil, repairs, err
	}
	return result, repairs, nil
}

func repairEntries(properties map[string]jsontext.Value, entries []objectEntry) ([]objectEntry, int, error) {
	repairs := 0
	kept := make([]objectEntry, 0, len(entries))
	for _, entry := range entries {
		property, ok := properties[entry.name]
		if !ok {
			repairs++
			if repairs > maxArgumentRepairs {
				return nil, repairs, errTooManyRepairs
			}
			continue
		}
		value, changed, err := repairValue(entry.value, property)
		if err != nil {
			return nil, repairs, err
		}
		if changed {
			repairs++
		}
		if repairs > maxArgumentRepairs {
			return nil, repairs, errTooManyRepairs
		}
		kept = append(kept, objectEntry{name: entry.name, value: value})
	}
	return kept, repairs, nil
}

func schemaProperties(schema jsontext.Value) (map[string]jsontext.Value, error) {
	entries, err := readObject(schema)
	if err != nil {
		return nil, fmt.Errorf("repair: read schema: %w", err)
	}
	properties := make(map[string]jsontext.Value)
	for _, entry := range entries {
		if entry.name != "properties" {
			continue
		}
		children, err := readObject(entry.value)
		if err != nil {
			return nil, fmt.Errorf("repair: read schema properties: %w", err)
		}
		for _, child := range children {
			properties[child.name] = child.value
		}
	}
	return properties, nil
}

func repairValue(raw jsontext.Value, schema jsontext.Value) (jsontext.Value, bool, error) {
	if !isJSONString(raw) {
		return raw, false, nil
	}
	allowed, err := schemaTypes(schema)
	if err != nil {
		return nil, false, err
	}
	if len(allowed) == 0 || allowed["string"] {
		return raw, false, nil
	}
	inner, err := decodeString(raw)
	if err != nil {
		return nil, false, fmt.Errorf("repair: decode string: %w", err)
	}
	candidate := jsontext.Value(inner)
	kind := valueKind(candidate)
	if !candidate.IsValid() || !repairableKind(kind) || !allowedDecodedKind(allowed, kind, candidate) {
		return raw, false, nil
	}
	return candidate, true, nil
}

func repairableKind(kind string) bool {
	switch kind {
	case "object", "array", "number", "boolean":
		return true
	default:
		return false
	}
}

func allowedDecodedKind(allowed map[string]bool, kind string, candidate jsontext.Value) bool {
	if allowed[kind] {
		return true
	}
	return kind == "number" && allowed["integer"] && !bytes.ContainsAny(candidate, ".eE")
}

func schemaTypes(schema jsontext.Value) (map[string]bool, error) {
	entries, err := readObject(schema)
	if err != nil {
		return nil, fmt.Errorf("repair: read property schema: %w", err)
	}
	for _, entry := range entries {
		if entry.name == "type" {
			return schemaTypeValue(entry.value)
		}
	}
	return nil, nil
}

func schemaTypeValue(raw jsontext.Value) (map[string]bool, error) {
	if isJSONString(raw) {
		value, err := decodeString(raw)
		if err != nil {
			return nil, fmt.Errorf("repair: decode schema type: %w", err)
		}
		return map[string]bool{value: true}, nil
	}
	items, err := readArray(raw)
	if err != nil {
		return nil, fmt.Errorf("repair: read schema types: %w", err)
	}
	result := make(map[string]bool, len(items))
	for _, item := range items {
		value, err := decodeString(item)
		if err != nil {
			return nil, fmt.Errorf("repair: decode schema type: %w", err)
		}
		result[value] = true
	}
	return result, nil
}

func readObject(raw jsontext.Value) ([]objectEntry, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, fmt.Errorf("repair: read object start: %w", err)
	}
	if start.Kind() != '{' {
		return nil, errors.New("repair: expected object")
	}
	entries := make([]objectEntry, 0, 8)
	for decoder.PeekKind() != '}' {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, fmt.Errorf("repair: read object name: %w", err)
		}
		nameText := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, fmt.Errorf("repair: read object value: %w", err)
		}
		entries = append(entries, objectEntry{name: nameText, value: value.Clone()})
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, fmt.Errorf("repair: read object end: %w", err)
	}
	return entries, nil
}

func readArray(raw jsontext.Value) ([]jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, fmt.Errorf("repair: read array start: %w", err)
	}
	if start.Kind() != '[' {
		return nil, errors.New("repair: expected array")
	}
	items := make([]jsontext.Value, 0, 4)
	for decoder.PeekKind() != ']' {
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, fmt.Errorf("repair: read array value: %w", err)
		}
		items = append(items, value.Clone())
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, fmt.Errorf("repair: read array end: %w", err)
	}
	return items, nil
}

func writeObject(entries []objectEntry) (jsontext.Value, error) {
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return nil, fmt.Errorf("repair: write object start: %w", err)
	}
	for _, entry := range entries {
		if err := encoder.WriteToken(jsontext.String(entry.name)); err != nil {
			return nil, fmt.Errorf("repair: write object name: %w", err)
		}
		if err := encoder.WriteValue(entry.value); err != nil {
			return nil, fmt.Errorf("repair: write object value: %w", err)
		}
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return nil, fmt.Errorf("repair: write object end: %w", err)
	}
	return jsontext.Value(bytes.TrimSpace(out.Bytes())), nil
}

func isJSONString(value jsontext.Value) bool {
	return jsontext.NewDecoder(bytes.NewReader(value)).PeekKind() == '"'
}

func decodeString(value jsontext.Value) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("read string: %w", err)
	}
	if token.Kind() != '"' {
		return "", errors.New("expected string")
	}
	return token.String(), nil
}

func valueKind(value jsontext.Value) string {
	switch jsontext.NewDecoder(bytes.NewReader(value)).PeekKind() {
	case jsontext.KindBeginObject:
		return "object"
	case jsontext.KindBeginArray:
		return "array"
	case jsontext.KindTrue, jsontext.KindFalse:
		return "boolean"
	case jsontext.KindNull:
		return "null"
	case jsontext.KindNumber:
		return "number"
	case jsontext.KindInvalid, jsontext.KindString, jsontext.KindEndObject, jsontext.KindEndArray:
		return ""
	}
	return ""
}
