package wire

import (
	"bytes"
	"encoding/binary"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"math"

	"google.golang.org/protobuf/encoding/protowire"
)

const maxValueDepth = 64

// source: https://protobuf.dev/reference/protobuf/google.protobuf/#value
const (
	valueNull   = 1
	valueNumber = 2
	valueString = 3
	valueBool   = 4
	valueStruct = 5
	valueList   = 6
)

func AppendValueFromJSON(dst []byte, value jsontext.Value) ([]byte, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	out, err := appendJSONValue(dst, decoder, 0)
	if err != nil {
		return nil, fmt.Errorf("cursor value from JSON: %w", err)
	}
	if _, err := decoder.ReadToken(); err == nil {
		return nil, errors.New("cursor value from JSON: trailing data")
	}
	return out, nil
}

func appendJSONValue(dst []byte, decoder *jsontext.Decoder, depth int) ([]byte, error) {
	if depth > maxValueDepth {
		return nil, errors.New("JSON nesting exceeds 64 levels")
	}
	kind := decoder.PeekKind()
	if kind == jsontext.KindBeginObject {
		return appendNested(dst, valueStruct, func(dst []byte) ([]byte, error) { return appendJSONObject(dst, decoder, depth) })
	}
	if kind == jsontext.KindBeginArray {
		return appendNested(dst, valueList, func(dst []byte) ([]byte, error) { return appendJSONArray(dst, decoder, depth) })
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return nil, err
	}
	return appendJSONScalar(dst, token)
}

func appendJSONScalar(dst []byte, token jsontext.Token) ([]byte, error) {
	kind := token.Kind()
	if kind == jsontext.KindNull {
		dst = protowire.AppendTag(dst, valueNull, protowire.VarintType)
		return protowire.AppendVarint(dst, 0), nil
	}
	if kind == jsontext.KindTrue || kind == jsontext.KindFalse {
		dst = protowire.AppendTag(dst, valueBool, protowire.VarintType)
		return protowire.AppendVarint(dst, protowire.EncodeBool(token.Bool())), nil
	}
	if kind == jsontext.KindString {
		dst = protowire.AppendTag(dst, valueString, protowire.BytesType)
		return protowire.AppendString(dst, token.String()), nil
	}
	if kind != jsontext.KindNumber {
		return nil, fmt.Errorf("unexpected JSON token %v", kind)
	}
	number, err := token.Float()
	if err != nil {
		return nil, fmt.Errorf("JSON number: %w", err)
	}
	dst = protowire.AppendTag(dst, valueNumber, protowire.Fixed64Type)
	return protowire.AppendFixed64(dst, math.Float64bits(number)), nil
}

func appendJSONObject(dst []byte, decoder *jsontext.Decoder, depth int) ([]byte, error) {
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	for decoder.PeekKind() != '}' {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, err
		}
		dst, err = appendNested(dst, 1, func(dst []byte) ([]byte, error) {
			dst = protowire.AppendTag(dst, 1, protowire.BytesType)
			dst = protowire.AppendString(dst, name.String())
			return appendNested(dst, 2, func(dst []byte) ([]byte, error) { return appendJSONValue(dst, decoder, depth+1) })
		})
		if err != nil {
			return nil, err
		}
	}
	_, err := decoder.ReadToken()
	return dst, err
}

func appendJSONArray(dst []byte, decoder *jsontext.Decoder, depth int) ([]byte, error) {
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	for decoder.PeekKind() != ']' {
		var err error
		dst, err = appendNested(dst, 1, func(dst []byte) ([]byte, error) { return appendJSONValue(dst, decoder, depth+1) })
		if err != nil {
			return nil, err
		}
	}
	_, err := decoder.ReadToken()
	return dst, err
}

func appendNested(dst []byte, n protowire.Number, encode func([]byte) ([]byte, error)) ([]byte, error) {
	dst = protowire.AppendTag(dst, n, protowire.BytesType)
	start := len(dst)
	dst, err := encode(dst)
	if err != nil {
		return nil, err
	}
	size := len(dst) - start
	prefix := varintSize(size)
	var pad [binary.MaxVarintLen64]byte
	dst = append(dst, pad[:prefix]...)
	copy(dst[start+prefix:], dst[start:start+size])
	writeVarint(dst[start:start+prefix], size)
	return dst, nil
}

func ValueJSON(b []byte) (jsontext.Value, error) {
	var buffer bytes.Buffer
	encoder := jsontext.NewEncoder(&buffer)
	if err := writeValueJSON(encoder, b, 0); err != nil {
		return nil, fmt.Errorf("cursor value to JSON: %w", err)
	}
	return bytes.TrimRight(buffer.Bytes(), "\n"), nil
}

func writeValueJSON(encoder *jsontext.Encoder, b []byte, depth int) error {
	if depth > maxValueDepth {
		return errors.New("value nesting exceeds 64 levels")
	}
	var last field
	found := false
	err := consumeFields(b, "cursor value", func(f field) error {
		if f.number >= valueNull && f.number <= valueList {
			last, found = f, true
		}
		return nil
	})
	if err != nil {
		return err
	}
	if !found {
		return encoder.WriteToken(jsontext.Null)
	}
	return writeValueField(encoder, last, depth)
}

func writeValueField(encoder *jsontext.Encoder, f field, depth int) error {
	switch f.number {
	case valueNumber:
		if err := requireKind(f, protowire.Fixed64Type, "cursor value"); err != nil {
			return err
		}
		bits, _ := protowire.ConsumeFixed64(f.raw)
		return encoder.WriteToken(jsontext.Float(math.Float64frombits(bits)))
	case valueString:
		if err := requireKind(f, protowire.BytesType, "cursor value"); err != nil {
			return err
		}
		return encoder.WriteToken(jsontext.String(string(f.data)))
	case valueBool:
		var value bool
		if err := setBool(f, "cursor value", &value); err != nil {
			return err
		}
		return encoder.WriteToken(jsontext.Bool(value))
	case valueStruct:
		return writeStructJSON(encoder, f, depth)
	case valueList:
		return writeListJSON(encoder, f, depth)
	}
	return encoder.WriteToken(jsontext.Null)
}

func writeStructJSON(encoder *jsontext.Encoder, f field, depth int) error {
	if err := requireKind(f, protowire.BytesType, "cursor struct"); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	err := consumeFields(f.data, "cursor struct", func(entry field) error {
		if entry.number != 1 {
			return nil
		}
		if err := requireKind(entry, protowire.BytesType, "cursor struct"); err != nil {
			return err
		}
		return writeStructEntry(encoder, entry.data, depth)
	})
	if err != nil {
		return err
	}
	return encoder.WriteToken(jsontext.EndObject)
}

func writeStructEntry(encoder *jsontext.Encoder, entry []byte, depth int) error {
	var key, value []byte
	err := consumeFields(entry, "cursor struct entry", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "cursor struct entry", &key)
		case 2:
			return setBytes(f, "cursor struct entry", &value)
		}
		return nil
	})
	if err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.String(string(key))); err != nil {
		return err
	}
	return writeValueJSON(encoder, value, depth+1)
}

func writeListJSON(encoder *jsontext.Encoder, f field, depth int) error {
	if err := requireKind(f, protowire.BytesType, "cursor list"); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.BeginArray); err != nil {
		return err
	}
	err := consumeFields(f.data, "cursor list", func(item field) error {
		if item.number != 1 {
			return nil
		}
		if err := requireKind(item, protowire.BytesType, "cursor list"); err != nil {
			return err
		}
		return writeValueJSON(encoder, item.data, depth+1)
	})
	if err != nil {
		return err
	}
	return encoder.WriteToken(jsontext.EndArray)
}
