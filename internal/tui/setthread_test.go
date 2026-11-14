package tui

import (
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestThreadSwitchKeepsFollowups(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.thread.id = "t1"
	m.queue = []string{"a", "b"}
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	if len(m.queue) != 2 {
		t.Fatalf("followups after /new = %q, want both kept", m.queue)
	}
	assertContains(t, m.View().Content, "2 follow-ups moved to")
	assertContains(t, m.notice, "2 follow-ups moved to")
}

func TestSameThreadResumeNoticesHeldFollowups(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.thread.id = "t1"
	m.queue = []string{"a", "b"}
	m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: "t1"}}})
	assertContains(t, m.notice, "2 follow-ups moved to")
	if len(m.queue) != 2 {
		t.Fatalf("followups after a same-thread resume = %q, want both kept", m.queue)
	}
}
