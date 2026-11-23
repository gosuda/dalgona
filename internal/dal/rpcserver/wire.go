package rpcserver

import (
	"bytes"
	"encoding/json/jsontext"
	"strconv"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func emptyObject() jsontext.Value { return jsontext.Value("{}") }

func decodeParams(params jsontext.Value, member rpc.MemberFunc) error {
	if len(params) == 0 {
		return nil
	}
	d := jsontext.NewDecoder(bytes.NewReader(params))
	if err := rpc.ReadMembers(d, member); err != nil {
		return invalidParams(err.Error())
	}
	return nil
}

func readPlain(d *jsontext.Decoder) (any, error) {
	if d.PeekKind() == jsontext.KindBeginObject {
		table := map[string]any{}
		err := rpc.ReadMembers(d, func(name string, d *jsontext.Decoder) error {
			v, err := readPlain(d)
			table[name] = v
			return err
		})
		return table, err
	}
	if d.PeekKind() == jsontext.KindBeginArray {
		var list []any
		err := rpc.ReadArray(d, func() error {
			v, err := readPlain(d)
			list = append(list, v)
			return err
		})
		return list, err
	}
	return readScalar(d)
}

func readScalar(d *jsontext.Decoder) (any, error) {
	t, ok, err := rpc.ReadToken(d, jsontext.KindString, jsontext.KindTrue, jsontext.KindFalse, jsontext.KindNumber)
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
