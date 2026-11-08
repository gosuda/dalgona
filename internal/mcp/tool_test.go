package mcp

import (
	"testing"
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
