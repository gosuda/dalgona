package rpc

import (
	"runtime"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestSpilledOutputCarriesSpillAndReadsBack(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("needs POSIX printf")
	}
	r := newRig(t, config.ApprovalYolo)
	r.llm.push(llmReply{body: sseCall("call-1", "exec", `{"command":"printf '%070000d' 0"}`)}, llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "print it")
	log := readTurn(t, c.collect())
	item := findItem(log.completed, "call-1")
	if item.Spill == nil || item.Spill.Bytes != 70000 {
		t.Fatalf("commandExecution spill = %+v, want 70000 bytes", item.Spill)
	}
	var out strings.Builder
	var offset int64
	for range 100 {
		var page struct {
			Text  string `json:"text"`
			Next  int64  `json:"next"`
			Total int64  `json:"total"`
		}
		c.ok("dalgona/item/output/read", map[string]any{"threadId": threadID, "itemId": "call-1", "offset": offset}, &page)
		out.WriteString(page.Text)
		offset = page.Next
		if page.Next == page.Total {
			break
		}
	}
	if out.Len() != 70000 || strings.Trim(out.String(), "0") != "" {
		t.Fatalf("paged output has %d bytes, want 70000 zero characters", out.Len())
	}
}
