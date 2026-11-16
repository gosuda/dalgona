package rpc

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strconv"
)

type out struct {
	buf *bytes.Buffer
	enc *jsontext.Encoder
	err error
}

func newOut() *out {
	buf := &bytes.Buffer{}
	return &out{buf: buf, enc: jsontext.NewEncoder(buf)}
}

func outTo(enc *jsontext.Encoder) *out { return &out{enc: enc} }

type writer interface{ write(o *out) }

func encode(w writer) (jsontext.Value, error) {
	o := newOut()
	w.write(o)
	return o.bytes()
}

func marshalTo(enc *jsontext.Encoder, w writer) error {
	o := outTo(enc)
	w.write(o)
	return o.err
}

func (o *out) token(t jsontext.Token) {
	if o.err == nil {
		o.err = o.enc.WriteToken(t)
	}
}

func (o *out) value(v jsontext.Value) {
	if o.err == nil {
		o.err = o.enc.WriteValue(v)
	}
}

func (o *out) open()           { o.token(jsontext.BeginObject) }
func (o *out) close()          { o.token(jsontext.EndObject) }
func (o *out) openArray()      { o.token(jsontext.BeginArray) }
func (o *out) closeArray()     { o.token(jsontext.EndArray) }
func (o *out) key(name string) { o.token(jsontext.String(name)) }
func (o *out) text(v string)   { o.token(jsontext.String(v)) }
func (o *out) null()           { o.token(jsontext.Null) }

func (o *out) str(name, v string) {
	o.key(name)
	o.text(v)
}

func (o *out) optStr(name, v string) {
	if v != "" {
		o.str(name, v)
	}
}

func (o *out) int(name string, v int64) {
	o.key(name)
	o.token(jsontext.Int(v))
}

func (o *out) boolean(name string, v bool) {
	o.key(name)
	o.token(jsontext.Bool(v))
}

func (o *out) raw(name string, v jsontext.Value) {
	o.key(name)
	if len(v) == 0 {
		o.null()
		return
	}
	o.value(v)
}

func (o *out) empty(name string) {
	o.key(name)
	o.openArray()
	o.closeArray()
}

func (o *out) bytes() (jsontext.Value, error) {
	if o.err != nil {
		return nil, fmt.Errorf("encode rpc message: %w", o.err)
	}
	return jsontext.Value(bytes.TrimSuffix(o.buf.Bytes(), []byte{'\n'})), nil
}

func emptyObject() jsontext.Value { return jsontext.Value("{}") }

type memberFunc func(name string, d *jsontext.Decoder) error

func decodeParams(params jsontext.Value, member memberFunc) error {
	if len(params) == 0 {
		return nil
	}
	d := jsontext.NewDecoder(bytes.NewReader(params))
	if err := readMembers(d, member); err != nil {
		return invalidParams(err.Error())
	}
	return nil
}

func readMembers(d *jsontext.Decoder, member memberFunc) error {
	open, err := d.ReadToken()
	if err != nil {
		return err
	}
	if open.Kind() == jsontext.KindNull {
		return nil
	}
	if open.Kind() != jsontext.KindBeginObject {
		return errors.New("params must be an object")
	}
	for d.PeekKind() != jsontext.KindEndObject {
		name, readErr := d.ReadToken()
		if readErr != nil {
			return readErr
		}
		key := name.String()
		if memberErr := member(key, d); memberErr != nil {
			return fmt.Errorf("%s: %w", key, memberErr)
		}
	}
	_, err = d.ReadToken()
	return err
}

func readArray(d *jsontext.Decoder, element func() error) error {
	open, err := d.ReadToken()
	if err != nil {
		return err
	}
	if open.Kind() == jsontext.KindNull {
		return nil
	}
	if open.Kind() != jsontext.KindBeginArray {
		return errors.New("expected an array")
	}
	for d.PeekKind() != jsontext.KindEndArray {
		if elementErr := element(); elementErr != nil {
			return elementErr
		}
	}
	_, err = d.ReadToken()
	return err
}

func readToken(d *jsontext.Decoder, want ...jsontext.Kind) (jsontext.Token, bool, error) {
	t, err := d.ReadToken()
	if err != nil {
		return t, false, err
	}
	if t.Kind() == jsontext.KindNull {
		return t, false, nil
	}
	for _, kind := range want {
		if t.Kind() == kind {
			return t, true, nil
		}
	}
	return t, false, fmt.Errorf("unexpected JSON %s", t.Kind())
}

func readString(d *jsontext.Decoder) (string, error) {
	t, ok, err := readToken(d, jsontext.KindString)
	if !ok {
		return "", err
	}
	return t.String(), nil
}

func readBool(d *jsontext.Decoder) (bool, error) {
	t, ok, err := readToken(d, jsontext.KindTrue, jsontext.KindFalse)
	if !ok {
		return false, err
	}
	return t.Bool(), nil
}

func readInt(d *jsontext.Decoder) (int, error) {
	t, ok, err := readToken(d, jsontext.KindNumber)
	if !ok {
		return 0, err
	}
	n, err := strconv.Atoi(t.String())
	if err != nil {
		return 0, errors.New("expected an integer")
	}
	return n, nil
}

func readPlain(d *jsontext.Decoder) (any, error) {
	if d.PeekKind() == jsontext.KindBeginObject {
		table := map[string]any{}
		err := readMembers(d, func(name string, d *jsontext.Decoder) error {
			v, err := readPlain(d)
			table[name] = v
			return err
		})
		return table, err
	}
	if d.PeekKind() == jsontext.KindBeginArray {
		var list []any
		err := readArray(d, func() error {
			v, err := readPlain(d)
			list = append(list, v)
			return err
		})
		return list, err
	}
	return readScalar(d)
}

func readScalar(d *jsontext.Decoder) (any, error) {
	t, ok, err := readToken(d, jsontext.KindString, jsontext.KindTrue, jsontext.KindFalse, jsontext.KindNumber)
	if !ok {
		return nil, err
	}
	if t.Kind() == jsontext.KindString {
		return t.String(), nil
	}
	if t.Kind() != jsontext.KindNumber {
		return t.Bool(), nil
	}
	if n, parseErr := strconv.ParseInt(t.String(), 10, 64); parseErr == nil {
		return n, nil
	}
	return t.Float()
}
