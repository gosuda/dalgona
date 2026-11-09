package model

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
)

func ReadObjectFields(decoder *jsontext.Decoder, field func(string) error) error {
	if decoder.PeekKind() == 'n' {
		return decoder.SkipValue()
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("model: read object start: %w", err)
	}
	if token.Kind() != '{' {
		return errors.New("model: expected JSON object")
	}
	for decoder.PeekKind() != '}' {
		token, err := decoder.ReadToken()
		if err != nil {
			return fmt.Errorf("model: read object name: %w", err)
		}
		name := token.String()
		if err := field(name); err != nil {
			return fmt.Errorf("model: read field %s: %w", name, err)
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return fmt.Errorf("model: read object end: %w", err)
	}
	return nil
}

func ReadArrayItems(decoder *jsontext.Decoder, item func(int) error) error {
	if decoder.PeekKind() == 'n' {
		return decoder.SkipValue()
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("model: read array start: %w", err)
	}
	if token.Kind() != '[' {
		return errors.New("model: expected JSON array")
	}
	for index := 0; decoder.PeekKind() != ']'; index++ {
		if err := item(index); err != nil {
			return fmt.Errorf("model: read array item %d: %w", index, err)
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return fmt.Errorf("model: read array end: %w", err)
	}
	return nil
}

func ReadString(decoder *jsontext.Decoder) (string, error) {
	if decoder.PeekKind() == 'n' {
		return "", decoder.SkipValue()
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("model: read string: %w", err)
	}
	if token.Kind() != '"' {
		return "", errors.New("model: expected JSON string")
	}
	return token.String(), nil
}

func ReadInt(decoder *jsontext.Decoder) (int, error) {
	if decoder.PeekKind() == 'n' {
		return 0, decoder.SkipValue()
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, fmt.Errorf("model: read integer: %w", err)
	}
	number, err := token.Int()
	if err != nil {
		return 0, fmt.Errorf("model: read integer: %w", err)
	}
	return int(number), nil
}

func ReadBool(decoder *jsontext.Decoder) (bool, error) {
	if decoder.PeekKind() == 'n' {
		return false, decoder.SkipValue()
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return false, fmt.Errorf("model: read boolean: %w", err)
	}
	if token.Kind() != 't' && token.Kind() != 'f' {
		return false, errors.New("model: expected JSON boolean")
	}
	return token.Bool(), nil
}

func ReadScalar(decoder *jsontext.Decoder) (string, error) {
	if decoder.PeekKind() == 'n' {
		return "", decoder.SkipValue()
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("model: read scalar: %w", err)
	}
	if token.Kind() == '{' || token.Kind() == '[' {
		return "", errors.New("model: expected JSON scalar")
	}
	return token.String(), nil
}

func ReadValue(decoder *jsontext.Decoder) (jsontext.Value, error) {
	value, err := decoder.ReadValue()
	if err != nil {
		return nil, fmt.Errorf("model: read value: %w", err)
	}
	return value.Clone(), nil
}

func RequireJSONEOF(decoder *jsontext.Decoder) error {
	_, err := decoder.ReadValue()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("model: decode trailing JSON: %w", err)
	}
	return errors.New("model: multiple JSON values")
}

type JSONEncoder struct {
	encoder *jsontext.Encoder
	err     error
}

func NewJSONEncoder(writer io.Writer) *JSONEncoder {
	return &JSONEncoder{encoder: jsontext.NewEncoder(writer)}
}

func (e *JSONEncoder) Err() error {
	return e.err
}

func (e *JSONEncoder) token(token jsontext.Token) {
	if e.err == nil {
		e.err = e.encoder.WriteToken(token)
	}
}

func (e *JSONEncoder) RawToken(token jsontext.Token) { e.token(token) }
func (e *JSONEncoder) BeginObject()                  { e.token(jsontext.BeginObject) }
func (e *JSONEncoder) EndObject()                    { e.token(jsontext.EndObject) }
func (e *JSONEncoder) BeginArray()                   { e.token(jsontext.BeginArray) }
func (e *JSONEncoder) EndArray()                     { e.token(jsontext.EndArray) }
func (e *JSONEncoder) Key(key string) {
	e.token(jsontext.String(key))
}
func (e *JSONEncoder) String(value string) { e.token(jsontext.String(value)) }
func (e *JSONEncoder) Int(value int)       { e.token(jsontext.Int(int64(value))) }
func (e *JSONEncoder) Bool(value bool)     { e.token(jsontext.Bool(value)) }
func (e *JSONEncoder) Null()               { e.token(jsontext.Null) }

func (e *JSONEncoder) Value(value jsontext.Value) {
	if e.err == nil {
		e.err = e.encoder.WriteValue(value)
	}
}

func (e *JSONEncoder) StringKey(key, value string) {
	e.Key(key)
	e.String(value)
}

func (e *JSONEncoder) IntKey(key string, value int) {
	e.Key(key)
	e.Int(value)
}

func (e *JSONEncoder) BoolKey(key string, value bool) {
	e.Key(key)
	e.Bool(value)
}

func (e *JSONEncoder) ValueKey(key string, value jsontext.Value) {
	e.Key(key)
	e.Value(value)
}

func (e *JSONEncoder) FreeformSchema() {
	e.BeginObject()
	e.StringKey("type", "object")
	e.Key("properties")
	e.BeginObject()
	e.Key("input")
	e.BeginObject()
	e.StringKey("type", "string")
	e.EndObject()
	e.EndObject()
	e.Key("required")
	e.BeginArray()
	e.String("input")
	e.EndArray()
	e.BoolKey("additionalProperties", false)
	e.EndObject()
}

func FreeformToolNames(tools []Tool) map[string]bool {
	names := make(map[string]bool)
	for _, tool := range tools {
		if tool.Freeform {
			names[tool.Name] = true
		}
	}
	return names
}

func DecodeFreeformInput(raw jsontext.Value) (jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	var input jsontext.Value
	found := false
	err := ReadObjectFields(decoder, func(key string) error {
		if key != "input" {
			return decoder.SkipValue()
		}
		if decoder.PeekKind() != '"' {
			return errors.New("model: freeform input must be a string")
		}
		value, err := ReadString(decoder)
		input, found = jsontext.Value(value), true
		return err
	})
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, errors.New("model: freeform input is missing")
	}
	if err := RequireJSONEOF(decoder); err != nil {
		return nil, err
	}
	return input, nil
}

func ToolResultText(output jsontext.Value) string {
	if len(output) != 0 && output[0] == '"' {
		if text, err := ReadString(jsontext.NewDecoder(bytes.NewReader(output))); err == nil {
			return text
		}
	}
	return string(output)
}
