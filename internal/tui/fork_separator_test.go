package tui

import (
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestForkPrintsSeparatorForNewThread(t *testing.T) {
	t.Parallel()
	m := playTerminal(newModel(nil, Options{}, nil))
	m.screen.width = 80
	m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: "t1", Name: "Plan"}}})
	m, _ = drive(t, m, resultMsg{op: "fork", value: &rpc.ThreadForkResponse{Thread: rpc.Thread{ID: "t2", Name: "Branch"}}})
	m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: "t2", Name: "Branch"}}})
	rows := entriesWithKey(m, "thread:t2:2")
	if len(rows) != 1 {
		t.Fatalf("separator rows for the forked thread = %d, want 1", len(rows))
	}
	if rows[0].text != "── Branch ──" {
		t.Fatalf("separator text = %q, want %q", rows[0].text, "── Branch ──")
	}
}
