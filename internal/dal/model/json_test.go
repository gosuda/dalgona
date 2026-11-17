package model

import (
	"encoding/json/jsontext"
	"errors"
	"strings"
	"testing"
)

func TestReadObjectFieldsWrapsFieldErrorAfterDecoderReads(t *testing.T) {
	t.Parallel()
	want := errors.New("bad field")
	decoder := jsontext.NewDecoder(strings.NewReader(`{"name":{"nested":1}}`))
	err := ReadObjectFields(decoder, func(string) error {
		if _, err := decoder.ReadToken(); err != nil {
			return err
		}
		return want
	})
	if !errors.Is(err, want) || !strings.Contains(err.Error(), "read field name") {
		t.Fatalf("error = %v, want wrapped field error", err)
	}
}

func TestDecodeFreeformInput(t *testing.T) {
	t.Parallel()
	cases := []struct {
		raw     string
		want    string
		wantErr bool
	}{
		{raw: `{"input":"patch"}`, want: "patch"},
		{raw: `{"input":""}`, want: ""},
		{raw: `{"other":"x"}`, wantErr: true},
		{raw: `{"input":7}`, wantErr: true},
		{raw: `{"input":"a"} {}`, wantErr: true},
	}
	for _, test := range cases {
		got, err := DecodeFreeformInput(jsontext.Value(test.raw))
		if (err != nil) != test.wantErr || string(got) != test.want {
			t.Fatalf("DecodeFreeformInput(%s) = %q, %v", test.raw, got, err)
		}
	}
}

func TestClassifyAttemptIgnoresTransientTextInProviderBody(t *testing.T) {
	t.Parallel()
	withBody := AttemptResult{Status: 200, Body: []byte(`{"delta":"request timeout"}`), Err: errors.New("openai: refusal")}
	if got := classifyAttempt(withBody); got != Terminal {
		t.Fatalf("provider body verdict = %v, want Terminal", got)
	}
	transport := AttemptResult{Status: 200, Err: errors.New("read: connection reset by peer")}
	if got := classifyAttempt(transport); got != Retry {
		t.Fatalf("transport verdict = %v, want Retry", got)
	}
	idle := AttemptResult{Status: 200, Err: ErrStreamIdleTimeout}
	if got := classifyAttempt(idle); got != Retry {
		t.Fatalf("idle verdict = %v, want Retry", got)
	}
}
