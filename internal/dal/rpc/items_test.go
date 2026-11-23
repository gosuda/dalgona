package rpc

import (
	json "encoding/json/v2"
	"reflect"
	"testing"
)

func TestItemDecodeIgnoresMemberOrder(t *testing.T) {
	for _, tc := range []struct {
		raw  string
		want Item
	}{
		{`{"content":[{"type":"text","text":"hi"}],"id":"u","type":"userMessage"}`, Item{Type: KindUser, ID: "u", Text: "hi"}},
		{`{"summary":["think"],"content":[],"type":"reasoning","id":"r"}`, Item{Type: KindReasoning, ID: "r", Text: "think"}},
		{`{"contentItems":[{"type":"inputText","text":"out"}],"tool":"todo","type":"dynamicToolCall","id":"d","status":"completed"}`, Item{Type: KindDynamic, ID: "d", Tool: "todo", Status: StatusCompleted, Output: "out"}},
	} {
		var got Item
		if err := json.Unmarshal([]byte(tc.raw), &got); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(got, tc.want) {
			t.Fatalf("%s decoded %+v, want %+v", tc.raw, got, tc.want)
		}
	}
}
