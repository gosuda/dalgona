package script

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"math/big"
	"strconv"
	"strings"

	"go.starlark.net/starlark"
	"go.starlark.net/starlarkstruct"
)

const maxJSONDepth = 256

var errJSONDepth = errors.New("value is nested too deeply or contains a cycle")

func newEncoder(w io.Writer) *jsontext.Encoder {
	return jsontext.NewEncoder(w, jsontext.AllowInvalidUTF8(true))
}

func toJSON(value starlark.Value) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := newEncoder(&buf)
	if err := writeValue(enc, value, 0); err != nil {
		return nil, err
	}
	return jsontext.Value(bytes.TrimSuffix(buf.Bytes(), []byte("\n"))), nil
}

func kwargsJSON(kwargs []starlark.Tuple) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := newEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	for _, pair := range kwargs {
		key, _ := starlark.AsString(pair[0])
		if err := writeMember(enc, key, pair[1], 0); err != nil {
			return nil, err
		}
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return jsontext.Value(bytes.TrimSuffix(buf.Bytes(), []byte("\n"))), nil
}

func writeMember(enc *jsontext.Encoder, key string, value starlark.Value, depth int) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return writeValue(enc, value, depth)
}

func writeValue(enc *jsontext.Encoder, value starlark.Value, depth int) error {
	if depth > maxJSONDepth {
		return errJSONDepth
	}
	switch value := value.(type) {
	case starlark.NoneType:
		return enc.WriteToken(jsontext.Null)
	case starlark.Bool:
		return enc.WriteToken(jsontext.Bool(bool(value)))
	case starlark.Int:
		return writeInt(enc, value)
	case starlark.Float:
		return enc.WriteToken(jsontext.Float(float64(value)))
	case starlark.String:
		return enc.WriteToken(jsontext.String(string(value)))
	default:
		return writeCompound(enc, value, depth+1)
	}
}

func writeInt(enc *jsontext.Encoder, value starlark.Int) error {
	if small, ok := value.Int64(); ok {
		return enc.WriteToken(jsontext.Int(small))
	}
	return enc.WriteValue(jsontext.Value(value.String()))
}

func writeCompound(enc *jsontext.Encoder, value starlark.Value, depth int) error {
	switch value := value.(type) {
	case *starlark.Dict:
		return writeDict(enc, value, depth)
	case *starlarkstruct.Struct:
		return writeAttrs(enc, value, depth)
	case starlark.Indexable:
		return writeIndexable(enc, value, depth)
	default:
		return fmt.Errorf("cannot encode %s as JSON", value.Type())
	}
}

func writeDict(enc *jsontext.Encoder, dict *starlark.Dict, depth int) error {
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, item := range dict.Items() {
		key, ok := item[0].(starlark.String)
		if !ok {
			return fmt.Errorf("cannot encode dict key of type %s as JSON", item[0].Type())
		}
		if err := writeMember(enc, string(key), item[1], depth); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndObject)
}

func writeAttrs(enc *jsontext.Encoder, value *starlarkstruct.Struct, depth int) error {
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, name := range value.AttrNames() {
		field, err := value.Attr(name)
		if err != nil {
			return err
		}
		if err := writeMember(enc, name, field, depth); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndObject)
}

func writeIndexable(enc *jsontext.Encoder, seq starlark.Indexable, depth int) error {
	if err := enc.WriteToken(jsontext.BeginArray); err != nil {
		return err
	}
	for i := range seq.Len() {
		if err := writeValue(enc, seq.Index(i), depth); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndArray)
}

func fromJSON(raw jsontext.Value) (starlark.Value, error) {
	if len(raw) == 0 {
		return starlark.None, nil
	}
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	value, err := readValue(dec)
	if err != nil {
		return nil, fmt.Errorf("decode JSON: %w", err)
	}
	return value, nil
}

func readValue(dec *jsontext.Decoder) (starlark.Value, error) {
	tok, err := dec.ReadToken()
	if err != nil {
		return nil, err
	}
	switch tok.Kind() {
	case jsontext.KindNull:
		return starlark.None, nil
	case jsontext.KindFalse, jsontext.KindTrue:
		return starlark.Bool(tok.Bool()), nil
	case jsontext.KindString:
		return starlark.String(tok.String()), nil
	case jsontext.KindNumber:
		return number(tok.String())
	case jsontext.KindBeginObject:
		return readObject(dec)
	case jsontext.KindBeginArray:
		return readArray(dec)
	case jsontext.KindInvalid, jsontext.KindEndObject, jsontext.KindEndArray:
		return nil, fmt.Errorf("unexpected JSON token %s", tok.Kind())
	}
	return nil, fmt.Errorf("unexpected JSON token %s", tok.Kind())
}

func number(text string) (starlark.Value, error) {
	if strings.ContainsAny(text, ".eE") {
		f, err := strconv.ParseFloat(text, 64)
		if err != nil {
			return nil, fmt.Errorf("decode number %s: %w", text, err)
		}
		return starlark.Float(f), nil
	}
	if small, err := strconv.ParseInt(text, 10, 64); err == nil {
		return starlark.MakeInt64(small), nil
	}
	large, ok := new(big.Int).SetString(text, 10)
	if !ok {
		return nil, fmt.Errorf("decode number %s", text)
	}
	return starlark.MakeBigInt(large), nil
}

func readObject(dec *jsontext.Decoder) (starlark.Value, error) {
	dict := starlark.NewDict(4)
	for dec.PeekKind() != jsontext.KindEndObject {
		key, err := dec.ReadToken()
		if err != nil {
			return nil, err
		}
		name := starlark.String(key.String())
		value, err := readValue(dec)
		if err != nil {
			return nil, err
		}
		if err := dict.SetKey(name, value); err != nil {
			return nil, err
		}
	}
	if _, err := dec.ReadToken(); err != nil {
		return nil, err
	}
	return dict, nil
}

func readArray(dec *jsontext.Decoder) (starlark.Value, error) {
	var items []starlark.Value
	for dec.PeekKind() != jsontext.KindEndArray {
		value, err := readValue(dec)
		if err != nil {
			return nil, err
		}
		items = append(items, value)
	}
	if _, err := dec.ReadToken(); err != nil {
		return nil, err
	}
	return starlark.NewList(items), nil
}

func stringJSON(text string) jsontext.Value {
	value, _ := jsontext.AppendQuote(nil, text)
	return value
}
