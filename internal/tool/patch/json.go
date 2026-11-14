package patch

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"strconv"
)

func decodeObject(raw jsontext.Value) (map[string]jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewBuffer(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, err
	}
	if start.Kind() != '{' {
		return nil, errors.New("expected object")
	}
	out := make(map[string]jsontext.Value)
	for decoder.PeekKind() != '}' {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, err
		}
		key := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, err
		}
		if _, exists := out[key]; exists {
			return nil, fmt.Errorf("duplicate field %q", key)
		}
		out[key] = bytes.Clone(value)
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return nil, errors.New("trailing JSON data")
	}
	return out, nil
}

func decodeArray(raw jsontext.Value) ([]jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewBuffer(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, err
	}
	if start.Kind() != '[' {
		return nil, errors.New("expected array")
	}
	var out []jsontext.Value
	for decoder.PeekKind() != ']' {
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, err
		}
		out = append(out, bytes.Clone(value))
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	return out, nil
}

func optionalString(object map[string]jsontext.Value, name string) (string, bool, error) {
	value, ok := object[name]
	if !ok || bytes.Equal(bytes.TrimSpace(value), []byte("null")) {
		return "", false, nil
	}
	text, err := jsonString(value)
	return text, true, err
}

func optionalBool(object map[string]jsontext.Value, name string) (bool, error) {
	value, ok := object[name]
	if !ok || bytes.Equal(bytes.TrimSpace(value), []byte("null")) {
		return false, nil
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return false, err
	}
	if token.Kind() != 't' && token.Kind() != 'f' {
		return false, errors.New("expected boolean")
	}
	return token.Bool(), nil
}

func optionalInt(object map[string]jsontext.Value, name string) (int, bool, error) {
	value, ok := object[name]
	if !ok || bytes.Equal(bytes.TrimSpace(value), []byte("null")) {
		return 0, false, nil
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, true, err
	}
	if token.Kind() != '0' {
		return 0, true, errors.New("expected integer")
	}
	number, err := strconv.Atoi(token.String())
	return number, true, err
}
