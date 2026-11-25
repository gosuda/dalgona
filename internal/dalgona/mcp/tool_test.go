package mcp

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
)

func TestDecodeToolResult(t *testing.T) {
	t.Parallel()
	cases := []toolResultCase{
		{name: "text blocks join", raw: `{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}],"isError":false}`, wantOutput: "a\nb", wantOK: true},
		{name: "empty text blocks stay separate", raw: `{"content":[{"type":"text","text":""},{"type":"text","text":""}]}`, wantOutput: "\n", wantOK: true},
		{name: "empty first text block keeps separator", raw: `{"content":[{"type":"text","text":""},{"type":"text","text":"b"}]}`, wantOutput: "\nb", wantOK: true},
		{name: "isError marks failure", raw: `{"content":[{"type":"text","text":"boom"}],"isError":true}`, wantOutput: "boom"},
		{name: "image becomes placeholder", raw: `{"content":[{"type":"image","data":"aGk=","mimeType":"image/jpeg"}]}`, wantOutput: "[image image/jpeg]", wantOK: true},
		{name: "unnamed link", raw: `{"content":[{"type":"resource_link","uri":"file:///x"}]}`, wantOutput: "[resource file:///x]", wantOK: true},
		{name: "named link", raw: `{"content":[{"type":"resource_link","uri":"file:///x","name":"x"}]}`, wantOutput: "[resource x: file:///x]", wantOK: true},
		{name: "unknown block type", raw: `{"content":[{"type":"audio","data":"x","mimeType":"audio/wav"}]}`, wantOutput: "[unsupported content audio]", wantOK: true},
		{name: "empty content", raw: `{"content":[]}`, wantOK: true},
		{name: "missing content", raw: `{"isError":false}`, wantError: true},
		{name: "non boolean isError", raw: `{"content":[],"isError":"yes"}`, wantError: true},
		{name: "non string block type", raw: `{"content":[{"type":42}]}`, wantError: true},
		{name: "array is not a result", raw: `[]`, wantError: true},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			t.Parallel()
			output, ok, err := decodeToolResult([]byte(testCase.raw))
			assertToolResult(t, testCase, output, ok, err)
		})
	}
}

type toolResultCase struct {
	name       string
	raw        string
	wantOutput string
	wantOK     bool
	wantError  bool
}

func assertToolResult(t *testing.T, testCase toolResultCase, output []byte, ok bool, err error) {
	t.Helper()
	if testCase.wantError {
		if err == nil {
			t.Fatalf("result = (%q, %v), want an error", output, ok)
		}
		return
	}
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if ok != testCase.wantOK || string(output) != testCase.wantOutput {
		t.Fatalf("result = (%q, %v), want (%q, %v)", output, ok, testCase.wantOutput, testCase.wantOK)
	}
}

type captureTransport struct {
	method string
	name   string
	result string
}

func (c *captureTransport) request(_ context.Context, _ uint64, method string, params jsontext.Value) (jsontext.Value, error) {
	c.method = method
	var body struct {
		Name string `json:"name"`
	}
	if err := json.Unmarshal(params, &body); err != nil {
		return nil, err
	}
	c.name = body.Name
	return jsontext.Value(c.result), nil
}

func (c *captureTransport) notify(context.Context, string) error { return nil }

func (c *captureTransport) Close() error { return nil }

func TestRewrittenToolKeepsOriginalCallName(t *testing.T) {
	transport := &captureTransport{result: `{"content":[{"type":"text","text":"ok"}]}`}
	client := &Client{cfg: Config{Name: "plug:srv"}, transport: transport}
	adapted, err := client.adaptTool([]byte(`{"name":"a:b","inputSchema":{"type":"object"}}`))
	if err != nil {
		t.Fatalf("adapt: %v", err)
	}
	requireAdaptedToolName(t, adapted)
	requireRewrittenToolCall(t, transport, adapted)
}

func requireAdaptedToolName(t *testing.T, adapted tool.Tool) {
	t.Helper()
	if got := adapted.Spec().Name; !safeNamePattern.MatchString(got) || len(got) > maxToolName || got == "plug:srv__a:b" {
		t.Fatalf("spec name = %q, want a rewritten provider-safe name", got)
	}
	if got := LocalToolName(adapted); got != "a:b" {
		t.Fatalf("local name = %q, want the original", got)
	}
}

func requireRewrittenToolCall(t *testing.T, transport *captureTransport, adapted tool.Tool) {
	t.Helper()
	result, err := adapted.Run(t.Context(), tool.Call{Args: jsontext.Value(`{}`)}, tool.Env{})
	if err != nil || !result.OK || string(result.Output) != "ok" {
		t.Fatalf("run = (%q, %v, %v), want (ok, true, nil)", result.Output, result.OK, err)
	}
	if transport.method != "tools/call" || transport.name != "a:b" {
		t.Fatalf("call = %s %q, want tools/call a:b", transport.method, transport.name)
	}
}
