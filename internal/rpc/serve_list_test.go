package rpc

import "testing"

func TestThreadListRejectsNegativeLimit(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	var page ThreadListResponse
	c.ok("thread/list", map[string]any{"limit": 1}, &page)
	c.fails("thread/list", map[string]any{"limit": -5}, -32602, "invalid limit -5")
	c.fails("thread/list", map[string]any{"cursor": "0", "limit": -1}, -32602, "invalid limit -1")
}
