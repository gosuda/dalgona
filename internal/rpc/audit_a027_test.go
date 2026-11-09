package rpc

import (
	"math"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestAuditHugeListLimitClamped(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	startThread(t, r, c, nil)
	var list ThreadListResponse
	c.ok("thread/list", map[string]any{"cursor": "1", "limit": math.MaxInt64}, &list)
	if len(list.Data) != 0 {
		t.Fatalf("rows=%d, want 0", len(list.Data))
	}
}
