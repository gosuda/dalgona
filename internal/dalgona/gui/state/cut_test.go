package state

import (
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func firsts(in []envelope) []uint64 {
	out := make([]uint64, 0, len(in))
	for _, e := range in {
		out = append(out, e.first)
	}
	return out
}

func routeAll(t *testing.T, c *threadCut, in ...envelope) []uint64 {
	t.Helper()
	var now []uint64
	for _, e := range in {
		apply, err := c.route(e)
		if err != nil {
			t.Fatalf("route %d-%d: %v", e.first, e.last, err)
		}
		if apply {
			now = append(now, e.first)
		}
	}
	return now
}

func TestCutKeepsOnlyEnvelopesWhollyAfterADelayedSnapshot(t *testing.T) {
	var c threadCut
	c.reopen()
	if now := routeAll(t, &c, span(3, 3), span(4, 6), span(7, 7), span(8, 9)); len(now) != 0 {
		t.Fatalf("applied %v before the first snapshot, want everything buffered", now)
	}
	apply, err := c.establish(6)
	if err != nil {
		t.Fatal(err)
	}
	if got, want := firsts(apply), []uint64{7, 8}; !slices.Equal(got, want) {
		t.Fatalf("applied %v after cut 6, want %v", got, want)
	}
	if now := routeAll(t, &c, span(5, 5), span(10, 10)); !slices.Equal(now, []uint64{10}) {
		t.Fatalf("after the cut applied %v, want only 10", now)
	}
}

func TestCutRefusesAStraddlingEnvelope(t *testing.T) {
	var held threadCut
	held.reopen()
	routeAll(t, &held, span(4, 6))
	if _, err := held.establish(5); !errors.Is(err, rpc.ErrStreamBroken) {
		t.Fatalf("a buffered envelope straddling the cut returned %v", err)
	}
	var known threadCut
	known.reopen()
	if _, err := known.establish(5); err != nil {
		t.Fatal(err)
	}
	if _, err := known.route(span(5, 6)); !errors.Is(err, rpc.ErrStreamBroken) {
		t.Fatalf("a live envelope straddling the cut returned %v", err)
	}
}

func TestCutReplacementKeepsTheCurrentProjectionLive(t *testing.T) {
	var c threadCut
	c.reopen()
	if _, err := c.establish(2); err != nil {
		t.Fatal(err)
	}
	c.reopen()
	now := routeAll(t, &c, span(2, 2), span(3, 3), span(4, 5), span(6, 6))
	if want := []uint64{3, 4, 6}; !slices.Equal(now, want) {
		t.Fatalf("the old projection applied %v during replacement, want %v", now, want)
	}
	apply, err := c.establish(5)
	if err != nil {
		t.Fatal(err)
	}
	if got := firsts(apply); !slices.Equal(got, []uint64{6}) {
		t.Fatalf("the replacement applied %v after cut 5, want [6]", got)
	}
}

func TestCutIgnoresThreadsWithoutAnOpenSnapshot(t *testing.T) {
	var c threadCut
	if now := routeAll(t, &c, span(1, 1)); len(now) != 0 || len(c.held) != 0 {
		t.Fatalf("an unopened thread applied %v and held %d envelopes", now, len(c.held))
	}
}
