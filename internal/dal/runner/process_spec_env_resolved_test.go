package runner

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"testing"
)

func TestProcessSpecEnvResolvedEncoding(t *testing.T) {
	for _, resolved := range []bool{true, false} {
		raw, err := encodeProcessSpec(ProcessSpec{ID: "p", Argv: []string{"x"}, EnvResolved: resolved})
		if err != nil {
			t.Fatal(err)
		}
		var members map[string]any
		if unmarshalErr := json.Unmarshal(raw, &members); unmarshalErr != nil {
			t.Fatal(unmarshalErr)
		}
		value, present := members["envResolved"]
		if present != resolved || (present && value != true) {
			t.Fatalf("resolved=%v encoded envResolved=%v present=%v in %s", resolved, value, present, raw)
		}
		decoded, decodeErr := decodeProcessSpec(raw)
		if decodeErr != nil {
			t.Fatal(decodeErr)
		}
		if decoded.EnvResolved != resolved {
			t.Fatalf("round trip of resolved=%v decoded %v", resolved, decoded.EnvResolved)
		}
	}
}

func TestProcessSpecEnvResolvedDecoding(t *testing.T) {
	const head = `{"processId":"p","argv":["x"]`
	for _, row := range []struct {
		name   string
		params string
		want   bool
		reject bool
	}{
		{name: "absent", params: head + `}`},
		{name: "true", params: head + `,"envResolved":true}`, want: true},
		{name: "false", params: head + `,"envResolved":false}`},
		{name: "string", params: head + `,"envResolved":"true"}`, reject: true},
		{name: "number", params: head + `,"envResolved":1}`, reject: true},
		{name: "null", params: head + `,"envResolved":null}`, reject: true},
		{name: "array", params: head + `,"envResolved":[true]}`, reject: true},
		{name: "object", params: head + `,"envResolved":{}}`, reject: true},
	} {
		t.Run(row.name, func(t *testing.T) {
			spec, err := decodeProcessSpec(jsontext.Value(row.params))
			if row.reject {
				if !errors.Is(err, errInvalidParams) {
					t.Fatalf("decode error = %v, want %v", err, errInvalidParams)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if spec.EnvResolved != row.want {
				t.Fatalf("EnvResolved = %v, want %v", spec.EnvResolved, row.want)
			}
		})
	}
}
