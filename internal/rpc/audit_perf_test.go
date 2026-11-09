package rpc

import (
	"net"
	"slices"
	"strconv"
	"testing"
	"time"
)

func notifyAuditDelta(tb testing.TB, c *Conn, itemID, delta string) {
	tb.Helper()
	params := map[string]string{"threadId": "t", "itemId": itemID, "delta": delta}
	if err := c.Notify("item/agentMessage/delta", params); err != nil {
		tb.Fatal(err)
	}
}

func newAuditConn(tb testing.TB) (*Conn, func()) {
	tb.Helper()
	server, peer := net.Pipe()
	c := NewConn(tb.Context(), server, nil)
	return c, func() {
		_ = peer.Close()
		_ = c.Close()
	}
}

func BenchmarkAuditDeltaMerge(b *testing.B) {
	c, done := newAuditConn(b)
	defer done()
	for range 1024 {
		notifyAuditDelta(b, c, "same", "x")
	}
	b.ResetTimer()
	for b.Loop() {
		notifyAuditDelta(b, c, "same", "y")
	}
}

func TestAuditDeltaMergeLatency(t *testing.T) {
	const distinct = 1024
	const pushes = 2000
	c, done := newAuditConn(t)
	defer done()
	for i := range distinct {
		notifyAuditDelta(t, c, "fill-"+strconv.Itoa(i), "x")
	}
	lat := make([]time.Duration, 0, pushes)
	for i := range pushes {
		start := time.Now()
		notifyAuditDelta(t, c, "new-"+strconv.Itoa(i), "y")
		lat = append(lat, time.Since(start))
	}
	slices.Sort(lat)
	pct := func(p float64) time.Duration { return lat[int(p*float64(len(lat)-1))] }
	t.Logf("notify push latency p50=%v p95=%v p99=%v max=%v", pct(0.50), pct(0.95), pct(0.99), lat[len(lat)-1])
}
