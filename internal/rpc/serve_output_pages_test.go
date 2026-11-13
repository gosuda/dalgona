package rpc

import (
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestOutputReadErrorsMapToInvalidParams(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("needs POSIX printf")
	}
	r := newRig(t, config.ApprovalYolo)
	r.llm.push(
		llmReply{body: sseCall("call-1", "exec", `{"command":"printf '%070000d' 0"}`)},
		llmReply{body: sseCall("call-2", "exec", `{"command":"echo hi"}`)},
		llmReply{body: sseText("done")},
	)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "print twice")
	c.collect("turn/completed")
	read := func(item string, offset int64, text string) {
		t.Helper()
		c.fails("dalgona/item/output/read", map[string]any{"threadId": threadID, "itemId": item, "offset": offset}, codeInvalidParams, text)
	}
	read("call-2", 0, "call call-2 has no spilled output")
	read("call-1", -1, "offset -1 is outside 0..70000")
	read("call-1", 70001, "offset 70001 is outside 0..70000")
}
