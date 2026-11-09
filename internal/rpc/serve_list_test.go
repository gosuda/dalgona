package rpc

import (
	"math"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestThreadListRejectsNegativeLimit(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	var page ThreadListResponse
	c.ok("thread/list", map[string]any{"limit": 1}, &page)
	c.fails("thread/list", map[string]any{"limit": -5}, -32602, "invalid limit -5")
	c.fails("thread/list", map[string]any{"cursor": "0", "limit": -1}, -32602, "invalid limit -1")
}

func TestThreadListClampsHugeLimit(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	startThread(t, r, c, nil)
	var list ThreadListResponse
	c.ok("thread/list", map[string]any{"cursor": "1", "limit": math.MaxInt64}, &list)
	if len(list.Data) != 0 {
		t.Fatalf("rows=%d, want 0", len(list.Data))
	}
}

func TestThreadListZeroLimitReturnsNoRows(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	startThread(t, r, c, nil)
	var list ThreadListResponse
	c.ok("thread/list", map[string]any{"limit": 0}, &list)
	if len(list.Data) != 0 {
		t.Fatalf("rows=%d, want 0", len(list.Data))
	}
	if list.NextCursor != "0" {
		t.Fatalf("cursor=%q, want 0", list.NextCursor)
	}
}
