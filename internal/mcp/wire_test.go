package mcp

import (
	"encoding/json/jsontext"
	"errors"
	"strings"
	"testing"
)

func TestEncodeMessage(t *testing.T) {
	t.Parallel()
	id := uint64(7)
	raw, err := encodeMessage(&id, "tools/list", jsontext.Value(`{"cursor":"c"}`))
	if err != nil {
		t.Fatalf("encode request: %v", err)
	}
	want := `{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{"cursor":"c"}}`
	if string(raw) != want {
		t.Fatalf("request = %s, want %s", raw, want)
	}
	notification, err := encodeMessage(nil, "notifications/initialized", nil)
	if err != nil {
		t.Fatalf("encode notification: %v", err)
	}
	wantNotification := `{"jsonrpc":"2.0","method":"notifications/initialized"}`
	if string(notification) != wantNotification {
		t.Fatalf("notification = %s, want %s", notification, wantNotification)
	}
	if _, err := encodeMessage(&id, "tools/list", jsontext.Value(`{"cursor":`)); err == nil {
		t.Fatal("encoded invalid params")
	}
}

func TestEncodeInitializeParams(t *testing.T) {
	t.Parallel()
	raw, err := encodeInitializeParams()
	if err != nil {
		t.Fatalf("encode initialize params: %v", err)
	}
	fields, err := decodeObject(raw)
	if err != nil {
		t.Fatalf("decode initialize params: %v", err)
	}
	assertMember(t, fields, "protocolVersion", protocolVersion)
	if _, ok := fields["capabilities"]; !ok {
		t.Fatal("initialize params have no capabilities member")
	}
	clientInfo, err := decodeObject(fields["clientInfo"])
	if err != nil {
		t.Fatalf("decode clientInfo: %v", err)
	}
	assertMember(t, clientInfo, "name", clientName)
	if _, err := decodeString(clientInfo["version"]); err != nil {
		t.Fatalf("clientInfo version: %v", err)
	}
}

func assertMember(t *testing.T, fields map[string]jsontext.Value, name, want string) {
	t.Helper()
	value, err := decodeString(fields[name])
	if err != nil || value != want {
		t.Fatalf("%s = %q, %v, want %q", name, value, err, want)
	}
}

func TestEncodeCallParams(t *testing.T) {
	t.Parallel()
	raw, err := encodeCallParams("echo", nil)
	if err != nil {
		t.Fatalf("encode call params: %v", err)
	}
	want := `{"name":"echo","arguments":{}}`
	if string(raw) != want {
		t.Fatalf("call params = %s, want %s", raw, want)
	}
	if _, err := encodeCallParams("echo", jsontext.Value(`[]`)); err == nil {
		t.Fatal("encoded array arguments")
	}
	if _, err := encodeCallParams("echo", jsontext.Value(`{"text":`)); err == nil {
		t.Fatal("encoded malformed arguments")
	}
}

func TestEncodeListParams(t *testing.T) {
	t.Parallel()
	raw, err := encodeListParams("")
	if err != nil {
		t.Fatalf("encode empty cursor: %v", err)
	}
	if string(raw) != `{}` {
		t.Fatalf("empty cursor params = %s", raw)
	}
	raw, err = encodeListParams("page2")
	if err != nil {
		t.Fatalf("encode cursor: %v", err)
	}
	if string(raw) != `{"cursor":"page2"}` {
		t.Fatalf("cursor params = %s", raw)
	}
}

type matchCase struct {
	name        string
	raw         string
	id          uint64
	wantMatched bool
	wantResult  string
	wantErr     string
	wantError   bool
}

func TestMatchResponse(t *testing.T) {
	t.Parallel()
	cases := []matchCase{
		{name: "result", raw: `{"jsonrpc":"2.0","id":1,"result":{"ok":true}}`, id: 1, wantMatched: true, wantResult: `{"ok":true}`},
		{name: "error", raw: `{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad params"}}`, id: 1, wantMatched: true, wantErr: "server error -32602: bad params"},
		{name: "id mismatch", raw: `{"jsonrpc":"2.0","id":2,"result":{}}`, id: 1},
		{name: "notification", raw: `{"jsonrpc":"2.0","method":"notifications/message"}`, id: 1},
		{name: "server request", raw: `{"jsonrpc":"2.0","id":1,"method":"sampling/createMessage","params":{}}`, id: 1},
		{name: "wrong version", raw: `{"jsonrpc":"1.0","id":1,"result":{}}`, id: 1, wantErr: "invalid JSON-RPC version"},
		{name: "missing version", raw: `{"id":1,"result":{}}`, id: 1, wantMatched: true, wantErr: "missing JSON-RPC version"},
		{name: "missing result and error", raw: `{"jsonrpc":"2.0","id":1}`, id: 1, wantMatched: true, wantErr: "neither result nor error"},
		{name: "result and error", raw: `{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-32602,"message":"bad params"}}`, id: 1, wantMatched: true, wantErr: "both result and error"},
		{name: "invalid json", raw: `{"jsonrpc":`, id: 1, wantError: true},
		{name: "not an object", raw: `1`, id: 1, wantError: true},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			t.Parallel()
			matched, result, err := matchResponse([]byte(testCase.raw), testCase.id)
			assertMatch(t, testCase, matched, result, err)
		})
	}
}

func assertMatch(t *testing.T, testCase matchCase, matched bool, result jsontext.Value, err error) {
	t.Helper()
	if testCase.wantErr != "" || testCase.wantError {
		assertMatchError(t, testCase, err)
		return
	}
	if err != nil {
		t.Fatalf("err = %v", err)
	}
	assertMatchPayload(t, testCase, matched, result)
}

func assertMatchError(t *testing.T, testCase matchCase, err error) {
	t.Helper()
	if testCase.wantErr != "" {
		if err == nil || !strings.Contains(err.Error(), testCase.wantErr) {
			t.Fatalf("err = %v, want it to contain %q", err, testCase.wantErr)
		}
		return
	}
	if err == nil {
		t.Fatal("expected an error")
	}
}

func assertMatchPayload(t *testing.T, testCase matchCase, matched bool, result jsontext.Value) {
	t.Helper()
	if matched != testCase.wantMatched {
		t.Fatalf("matched = %v, want %v", matched, testCase.wantMatched)
	}
	if testCase.wantResult != "" && string(result) != testCase.wantResult {
		t.Fatalf("result = %s, want %s", result, testCase.wantResult)
	}
}

func TestMatchResponseErrorType(t *testing.T) {
	t.Parallel()
	_, _, err := matchResponse([]byte(`{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad params"}}`), 1)
	if err == nil {
		t.Fatal("expected an rpc error")
	}
	failure, ok := errors.AsType[*rpcError](err)
	if !ok {
		t.Fatalf("error type = %T", err)
	}
	if failure.Code != -32602 || failure.Message != "bad params" {
		t.Fatalf("rpc error = %d %q", failure.Code, failure.Message)
	}
}

func TestDecodeObjectPreservesRawValues(t *testing.T) {
	t.Parallel()
	fields, err := decodeObject([]byte(`{ "a" : [1, 2] , "b": "x" }`))
	if err != nil {
		t.Fatalf("decode object: %v", err)
	}
	if string(fields["a"]) != `[1, 2]` {
		t.Fatalf("a = %s, want [1, 2]", fields["a"])
	}
	if string(fields["b"]) != `"x"` {
		t.Fatalf("b = %s, want \"x\"", fields["b"])
	}
	if _, err := decodeObject([]byte(`{"a":1} trailing`)); err == nil {
		t.Fatal("decoded trailing content")
	}
	if _, err := decodeObject([]byte(`[1]`)); err == nil {
		t.Fatal("decoded an array as an object")
	}
}

func TestDecodeScalarRejectsWrongKinds(t *testing.T) {
	t.Parallel()
	if _, err := decodeString([]byte(`1`)); err == nil {
		t.Fatal("decoded a number as a string")
	}
	if _, err := decodeBool([]byte(`"x"`)); err == nil {
		t.Fatal("decoded a string as a boolean")
	}
	if _, err := decodeUint([]byte(`-1`)); err == nil {
		t.Fatal("decoded a negative number as unsigned")
	}
	if _, err := decodeUint([]byte(`1 2`)); err == nil {
		t.Fatal("decoded trailing content as unsigned")
	}
	if _, err := decodeInt([]byte(`"1"`)); err == nil {
		t.Fatal("decoded a string as a number")
	}
	if _, err := decodeInt([]byte(`1.5`)); err == nil {
		t.Fatal("decoded a fraction as an integer")
	}
}
