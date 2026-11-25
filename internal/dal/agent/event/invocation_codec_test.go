package event_test

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"maps"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	invocationFingerprint = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
	invocationIdentity    = "thread-1/2/turn-4/req-3/call_9"
)

const invocationStartedWire = `{"type":"hashline.invocation_started","call_id":"call_9","turn_id":"turn-4","identity":"` +
	invocationIdentity + `","fingerprint":"` + invocationFingerprint +
	`","style":"hashline-enhanced","contract_version":1,"paths":["src/lib.rs","docs/read me.md"]}`

const invocationSettledMinimalWire = `{"type":"hashline.invocation_settled","identity":"` + invocationIdentity +
	`","ok":true,"output":"applied\n"}`

const invocationSettledFullWire = `{"type":"hashline.invocation_settled","identity":"` + invocationIdentity +
	`","ok":false,"output":"cut\n","spill":{"path":"/tmp/out.txt","bytes":70000,"sha256":"` + digest +
	`"},"meta":{"hashline":{"status":"rejected"}}}`

func invocationStarted() event.HashlineInvocationStarted {
	return event.HashlineInvocationStarted{
		CallID: "call_9", TurnID: "turn-4", Identity: invocationIdentity, Fingerprint: invocationFingerprint,
		Style: "hashline-enhanced", ContractVersion: 1, Paths: []string{"src/lib.rs", "docs/read me.md"},
	}
}

func invocationSettledMinimal() event.HashlineInvocationSettled {
	return event.HashlineInvocationSettled{Identity: invocationIdentity, OK: true, Output: "applied\n"}
}

func invocationSettledFull() event.HashlineInvocationSettled {
	return event.HashlineInvocationSettled{
		Identity: invocationIdentity, OK: false, Output: "cut\n",
		Spill: &event.Spill{Path: "/tmp/out.txt", Bytes: 70000, SHA256: digest},
		Meta:  jsontext.Value(`{"hashline":{"status":"rejected"}}`),
	}
}

func invocationMemberNames(t *testing.T, encoded []byte) []string {
	t.Helper()
	var members map[string]jsontext.Value
	if err := json.Unmarshal(encoded, &members); err != nil {
		t.Fatalf("encoded record is not one JSON object: %v\n%s", err, encoded)
	}
	return slices.Sorted(maps.Keys(members))
}

func invocationJSONEqual(t *testing.T, got, want []byte) bool {
	t.Helper()
	var left, right any
	if err := json.Unmarshal(got, &left); err != nil {
		t.Fatalf("got is not JSON: %v\n%s", err, got)
	}
	if err := json.Unmarshal(want, &right); err != nil {
		t.Fatalf("want is not JSON: %v\n%s", err, want)
	}
	return reflect.DeepEqual(left, right)
}

func invocationVariant(t *testing.T, wire, old, replacement string) string {
	t.Helper()
	if !strings.Contains(wire, old) {
		t.Fatalf("test fixture lacks %q", old)
	}
	return strings.Replace(wire, old, replacement, 1)
}

func invocationEncode(t *testing.T, value event.Event) []byte {
	t.Helper()
	encoded, err := event.Encode(value)
	if err != nil {
		t.Fatalf("Encode(%s): %v", value.Type(), err)
	}
	return encoded
}

func invocationRoundTrip(t *testing.T, original event.Event) {
	t.Helper()
	decoded, err := event.Decode(invocationEncode(t, original))
	if err != nil {
		t.Fatalf("Decode(%s): %v", original.Type(), err)
	}
	if !reflect.DeepEqual(decoded, original) {
		t.Fatalf("%s round trip\ngot:  %#v\nwant: %#v", original.Type(), decoded, original)
	}
}

func TestHashlineInvocationCodecDecodesSpecifiedRecords(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		wire string
		want event.Event
	}{
		{name: "started", wire: invocationStartedWire, want: invocationStarted()},
		{name: "settled minimal", wire: invocationSettledMinimalWire, want: invocationSettledMinimal()},
		{name: "settled with spill and meta", wire: invocationSettledFullWire, want: invocationSettledFull()},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, err := event.Decode([]byte(tc.wire))
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if !reflect.DeepEqual(got, tc.want) {
				t.Fatalf("Decode\ngot:  %#v\nwant: %#v", got, tc.want)
			}
		})
	}
	other := invocationVariant(t, invocationStartedWire, `"call_id":"call_9"`, `"call_id":"call_8"`)
	got, err := event.Decode([]byte(other))
	if err != nil {
		t.Fatalf("Decode control: %v", err)
	}
	if reflect.DeepEqual(got, event.Event(invocationStarted())) {
		t.Fatal("a record with another call_id decoded to the specified record")
	}
}

func TestHashlineInvocationCodecEncodesSpecifiedMembers(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		value event.Event
		wire  string
		names []string
	}{
		{
			name: "started", value: invocationStarted(), wire: invocationStartedWire,
			names: []string{"call_id", "contract_version", "fingerprint", "identity", "paths", "style", "turn_id", "type"},
		},
		{
			name: "settled minimal", value: invocationSettledMinimal(), wire: invocationSettledMinimalWire,
			names: []string{"identity", "ok", "output", "type"},
		},
		{
			name: "settled with spill and meta", value: invocationSettledFull(), wire: invocationSettledFullWire,
			names: []string{"identity", "meta", "ok", "output", "spill", "type"},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			encoded := invocationEncode(t, tc.value)
			if got := invocationMemberNames(t, encoded); !slices.Equal(got, tc.names) {
				t.Fatalf("members = %v, want %v", got, tc.names)
			}
			if !invocationJSONEqual(t, encoded, []byte(tc.wire)) {
				t.Fatalf("encoding differs from the specified record\ngot:  %s\nwant: %s", encoded, tc.wire)
			}
			if strings.HasSuffix(string(encoded), "\n") {
				t.Fatal("encoding ends with a newline")
			}
		})
	}
	changed := invocationStarted()
	changed.Style = "hashline-light"
	if invocationJSONEqual(t, invocationEncode(t, changed), []byte(invocationStartedWire)) {
		t.Fatal("a record with another style equals the specified record")
	}
}

func TestHashlineInvocationCodecRoundTrip(t *testing.T) {
	t.Parallel()
	started := invocationStarted()
	started.Paths = []string{"quote\"back\\slash.go", "tab\tname.go", "달/파일.go", "a b/c.go"}
	started.Style = "hashline-light"
	started.ContractVersion = 7
	exact := invocationSettledMinimal()
	exact.Output = strings.Repeat("x", event.MaxInlineBytes)
	empty := event.HashlineInvocationSettled{Identity: invocationIdentity, OK: true}
	projected := invocationSettledFull()
	projected.Output = strings.Repeat("y", event.MaxProjectionBytes)
	cases := []event.Event{
		invocationStarted(), started, invocationSettledMinimal(), invocationSettledFull(), exact, empty, projected,
	}
	for _, original := range cases {
		invocationRoundTrip(t, original)
	}
}

func TestHashlineInvocationCodecValidatesStarted(t *testing.T) {
	t.Parallel()
	half := strings.Repeat("a", event.MaxInlineBytes/2)
	cases := []struct {
		name    string
		mutate  func(*event.HashlineInvocationStarted)
		wantErr bool
	}{
		{name: "specified record", mutate: func(*event.HashlineInvocationStarted) {}},
		{name: "light style", mutate: func(e *event.HashlineInvocationStarted) { e.Style = "hashline-light" }},
		{name: "empty call id", mutate: func(e *event.HashlineInvocationStarted) { e.CallID = "" }, wantErr: true},
		{name: "empty turn id", mutate: func(e *event.HashlineInvocationStarted) { e.TurnID = "" }, wantErr: true},
		{name: "empty identity", mutate: func(e *event.HashlineInvocationStarted) { e.Identity = "" }, wantErr: true},
		{name: "empty fingerprint", mutate: func(e *event.HashlineInvocationStarted) { e.Fingerprint = "" }, wantErr: true},
		{name: "short fingerprint", mutate: func(e *event.HashlineInvocationStarted) { e.Fingerprint = e.Fingerprint[1:] }, wantErr: true},
		{
			name:    "upper-case fingerprint",
			mutate:  func(e *event.HashlineInvocationStarted) { e.Fingerprint = strings.ToUpper(e.Fingerprint) },
			wantErr: true,
		},
		{name: "legacy hashline style", mutate: func(e *event.HashlineInvocationStarted) { e.Style = "hashline" }, wantErr: true},
		{name: "empty style", mutate: func(e *event.HashlineInvocationStarted) { e.Style = "" }, wantErr: true},
		{name: "zero contract version", mutate: func(e *event.HashlineInvocationStarted) { e.ContractVersion = 0 }, wantErr: true},
		{name: "negative contract version", mutate: func(e *event.HashlineInvocationStarted) { e.ContractVersion = -1 }, wantErr: true},
		{name: "no paths", mutate: func(e *event.HashlineInvocationStarted) { e.Paths = nil }, wantErr: true},
		{name: "empty path", mutate: func(e *event.HashlineInvocationStarted) { e.Paths = []string{"a.go", ""} }, wantErr: true},
		{name: "paths at 64 KiB", mutate: func(e *event.HashlineInvocationStarted) { e.Paths = []string{half, half} }},
		{name: "paths over 64 KiB", mutate: func(e *event.HashlineInvocationStarted) { e.Paths = []string{half, half + "b"} }, wantErr: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			value := invocationStarted()
			tc.mutate(&value)
			if err := value.Validate(); (err != nil) != tc.wantErr {
				t.Fatalf("Validate() = %v, want error %t", err, tc.wantErr)
			}
			if _, err := event.Encode(value); (err != nil) != tc.wantErr {
				t.Fatalf("Encode() error = %v, want error %t", err, tc.wantErr)
			}
			if !tc.wantErr {
				invocationRoundTrip(t, value)
			}
		})
	}
}

func TestHashlineInvocationCodecValidatesSettled(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		mutate  func(*event.HashlineInvocationSettled)
		wantErr bool
	}{
		{name: "specified record", mutate: func(*event.HashlineInvocationSettled) {}},
		{name: "empty identity", mutate: func(e *event.HashlineInvocationSettled) { e.Identity = "" }, wantErr: true},
		{
			name:   "output at 64 KiB",
			mutate: func(e *event.HashlineInvocationSettled) { e.Output = strings.Repeat("x", event.MaxInlineBytes) },
		},
		{
			name:    "output over 64 KiB without spill",
			mutate:  func(e *event.HashlineInvocationSettled) { e.Output = strings.Repeat("x", event.MaxInlineBytes+1) },
			wantErr: true,
		},
		{
			name: "projection at its limit with spill",
			mutate: func(e *event.HashlineInvocationSettled) {
				e.Output = strings.Repeat("x", event.MaxProjectionBytes)
				e.Spill = &event.Spill{Path: "/tmp/out.txt", Bytes: 1 << 20, SHA256: digest}
			},
		},
		{
			name: "projection over its limit with spill",
			mutate: func(e *event.HashlineInvocationSettled) {
				e.Output = strings.Repeat("x", event.MaxProjectionBytes+1)
				e.Spill = &event.Spill{Path: "/tmp/out.txt", Bytes: 1 << 20, SHA256: digest}
			},
			wantErr: true,
		},
		{
			name:    "spill without path",
			mutate:  func(e *event.HashlineInvocationSettled) { e.Spill = &event.Spill{Bytes: 1, SHA256: digest} },
			wantErr: true,
		},
		{
			name: "spill with negative bytes",
			mutate: func(e *event.HashlineInvocationSettled) {
				e.Spill = &event.Spill{Path: "/p", Bytes: -1, SHA256: digest}
			},
			wantErr: true,
		},
		{
			name:    "spill with bad digest",
			mutate:  func(e *event.HashlineInvocationSettled) { e.Spill = &event.Spill{Path: "/p", Bytes: 1, SHA256: "abc"} },
			wantErr: true,
		},
		{
			name: "meta at 64 KiB",
			mutate: func(e *event.HashlineInvocationSettled) {
				e.Meta = jsontext.Value(`"` + strings.Repeat("m", event.MaxInlineBytes-2) + `"`)
			},
		},
		{
			name: "meta over 64 KiB",
			mutate: func(e *event.HashlineInvocationSettled) {
				e.Meta = jsontext.Value(`"` + strings.Repeat("m", event.MaxInlineBytes-1) + `"`)
			},
			wantErr: true,
		},
		{
			name:    "meta that is not JSON",
			mutate:  func(e *event.HashlineInvocationSettled) { e.Meta = jsontext.Value(`{"hashline":`) },
			wantErr: true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			value := invocationSettledMinimal()
			tc.mutate(&value)
			if err := value.Validate(); (err != nil) != tc.wantErr {
				t.Fatalf("Validate() = %v, want error %t", err, tc.wantErr)
			}
			if _, err := event.Encode(value); (err != nil) != tc.wantErr {
				t.Fatalf("Encode() error = %v, want error %t", err, tc.wantErr)
			}
			if !tc.wantErr {
				invocationRoundTrip(t, value)
			}
		})
	}
}

func TestHashlineInvocationCodecRejectsMalformedRecords(t *testing.T) {
	t.Parallel()
	started := func(old, replacement string) string {
		return invocationVariant(t, invocationStartedWire, old, replacement)
	}
	settled := func(old, replacement string) string {
		return invocationVariant(t, invocationSettledFullWire, old, replacement)
	}
	cases := []struct {
		name string
		raw  string
	}{
		{name: "started without call_id", raw: started(`"call_id":"call_9",`, ``)},
		{name: "started without turn_id", raw: started(`"turn_id":"turn-4",`, ``)},
		{name: "started without identity", raw: started(`"identity":"`+invocationIdentity+`",`, ``)},
		{name: "started without fingerprint", raw: started(`"fingerprint":"`+invocationFingerprint+`",`, ``)},
		{name: "started without style", raw: started(`"style":"hashline-enhanced",`, ``)},
		{name: "started without contract_version", raw: started(`"contract_version":1,`, ``)},
		{name: "started without paths", raw: started(`,"paths":["src/lib.rs","docs/read me.md"]`, ``)},
		{name: "started with zero contract_version", raw: started(`"contract_version":1`, `"contract_version":0`)},
		{name: "started with text contract_version", raw: started(`"contract_version":1`, `"contract_version":"1"`)},
		{name: "started with empty paths", raw: started(`["src/lib.rs","docs/read me.md"]`, `[]`)},
		{name: "started with text paths", raw: started(`["src/lib.rs","docs/read me.md"]`, `"src/lib.rs"`)},
		{name: "started with numeric path", raw: started(`"docs/read me.md"`, `7`)},
		{name: "started with empty path", raw: started(`"docs/read me.md"`, `""`)},
		{name: "started with short fingerprint", raw: started(invocationFingerprint, invocationFingerprint[1:])},
		{name: "started with upper-case fingerprint", raw: started(invocationFingerprint, strings.ToUpper(invocationFingerprint))},
		{name: "started with legacy style", raw: started(`"hashline-enhanced"`, `"hashline"`)},
		{name: "settled without identity", raw: settled(`"identity":"`+invocationIdentity+`",`, ``)},
		{name: "settled without ok", raw: settled(`"ok":false,`, ``)},
		{name: "settled without output", raw: settled(`"output":"cut\n",`, ``)},
		{name: "settled with text ok", raw: settled(`"ok":false`, `"ok":"false"`)},
		{name: "settled with spill missing sha256", raw: settled(`,"sha256":"`+digest+`"`, ``)},
		{name: "settled with spill bad sha256", raw: settled(digest, "abc")},
		{name: "settled with spill text bytes", raw: settled(`"bytes":70000`, `"bytes":"70000"`)},
		{name: "settled with spill that is not an object", raw: settled(`{"path":"/tmp/out.txt","bytes":70000,"sha256":"`+digest+`"}`, `"/tmp/out.txt"`)},
	}
	for _, tc := range cases {
		if _, err := event.Decode([]byte(tc.raw)); err == nil {
			t.Errorf("%s: Decode accepted %s", tc.name, tc.raw)
		}
	}
	for _, control := range []string{invocationStartedWire, invocationSettledMinimalWire, invocationSettledFullWire} {
		if _, err := event.Decode([]byte(control)); err != nil {
			t.Errorf("control record rejected: %v\n%s", err, control)
		}
	}
}
