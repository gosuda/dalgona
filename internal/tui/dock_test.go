package tui

import (
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestFollowupNoticeKeepsDockHeight(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t1"}}})
	m.queue = []string{"a", "b"}
	before := len(m.dockLines())
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock height after the follow-up notice = %d, want %d", got, before)
	}
}
