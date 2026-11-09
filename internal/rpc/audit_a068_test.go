package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestAuditExplicitZeroLimitReturnsNoRows(t *testing.T) {
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
