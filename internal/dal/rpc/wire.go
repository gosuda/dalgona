package rpc

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strconv"
)

type Writer struct {
	buf *bytes.Buffer
	enc *jsontext.Encoder
	err error
}

func NewWriter() *Writer {
	buf := &bytes.Buffer{}
	return &Writer{buf: buf, enc: jsontext.NewEncoder(buf)}
}

func outTo(enc *jsontext.Encoder) *Writer { return &Writer{enc: enc} }

func Marshal(w Encoder) (jsontext.Value, error) {
	o := NewWriter()
	w.Encode(o)
	return o.Bytes()
}

type Encoder interface{ Encode(o *Writer) }

func marshalTo(enc *jsontext.Encoder, w Encoder) error {
	o := outTo(enc)
	w.Encode(o)
	return o.err
}

func (o *Writer) Token(t jsontext.Token) {
	if o.err == nil {
		o.err = o.enc.WriteToken(t)
	}
}

func (o *Writer) Value(v jsontext.Value) {
	if o.err == nil {
		o.err = o.enc.WriteValue(v)
	}
}

func (o *Writer) Open() { o.Token(jsontext.BeginObject) }

func (o *Writer) Close() { o.Token(jsontext.EndObject) }

func (o *Writer) OpenArray() { o.Token(jsontext.BeginArray) }

func (o *Writer) CloseArray() { o.Token(jsontext.EndArray) }

func (o *Writer) Key(name string) { o.Token(jsontext.String(name)) }

func (o *Writer) Text(v string) { o.Token(jsontext.String(v)) }

func (o *Writer) Null() { o.Token(jsontext.Null) }

func (o *Writer) Str(name, v string) {
	o.Key(name)
	o.Text(v)
}

func (o *Writer) OptStr(name, v string) {
	if v != "" {
		o.Str(name, v)
	}
}

func (o *Writer) Int(name string, v int64) {
	o.Key(name)
	o.Token(jsontext.Int(v))
}

func (o *Writer) Bool(name string, v bool) {
	o.Key(name)
	o.Token(jsontext.Bool(v))
}

func (o *Writer) Raw(name string, v jsontext.Value) {
	o.Key(name)
	if len(v) == 0 {
		o.Null()
		return
	}
	o.Value(v)
}

func (o *Writer) Empty(name string) {
	o.Key(name)
	o.OpenArray()
	o.CloseArray()
}

func (o *Writer) Bytes() (jsontext.Value, error) {
	if o.err != nil {
		return nil, fmt.Errorf("encode rpc message: %w", o.err)
	}
	return jsontext.Value(bytes.TrimSuffix(o.buf.Bytes(), []byte{'\n'})), nil
}

type MemberFunc func(name string, d *jsontext.Decoder) error

func ReadMembers(d *jsontext.Decoder, member MemberFunc) error {
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

func ReadArray(d *jsontext.Decoder, element func() error) error {
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

func ReadToken(d *jsontext.Decoder, want ...jsontext.Kind) (jsontext.Token, bool, error) {
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

func ReadString(d *jsontext.Decoder) (string, error) {
	t, ok, err := ReadToken(d, jsontext.KindString)
	if !ok {
		return "", err
	}
	return t.String(), nil
}

func ReadBool(d *jsontext.Decoder) (bool, error) {
	t, ok, err := ReadToken(d, jsontext.KindTrue, jsontext.KindFalse)
	if !ok {
		return false, err
	}
	return t.Bool(), nil
}

func ReadInt(d *jsontext.Decoder) (int, error) {
	t, ok, err := ReadToken(d, jsontext.KindNumber)
	if !ok {
		return 0, err
	}
	n, err := strconv.Atoi(t.String())
	if err != nil {
		return 0, errors.New("expected an integer")
	}
	return n, nil
}
