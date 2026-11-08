package agent

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
)

type scriptedReads struct {
	runner.Runner
	reads []runner.Output
}

func (s *scriptedReads) Read(context.Context, string, int64) (runner.Output, error) {
	next := s.reads[0]
	s.reads = s.reads[1:]
	return next, nil
}

func drained(t *Thread) []string {
	var deltas []string
	for {
		select {
		case update := <-t.output.ch:
			deltas = append(deltas, update.Delta)
		default:
			return deltas
		}
	}
}

func TestOutputTeeEmitsEachByteOnceAtLineBoundaries(t *testing.T) {
	thread := &Thread{id: "t", host: &Host{}, output: outputRelay{ch: make(chan Update, outputBuffer)}}
	exit := &runner.Exit{}
	tee := thread.teeOutput(&scriptedReads{reads: []runner.Output{
		{Data: []byte("one\ntw"), Next: 6},
		{Data: []byte("o\nthr"), Next: 11},
		{Data: []byte("o\nthr"), Next: 11},
		{Data: []byte("ee"), Next: 13, Exit: exit},
		{Data: []byte("thr"), Next: 11, Exit: exit},
	}}, "p", "call")
	for range 5 {
		if _, err := tee.Read(t.Context(), "p", 0); err != nil {
			t.Fatal(err)
		}
	}
	tee.flush()
	deltas := drained(thread)
	if strings.Join(deltas, "|") != "one\n|two\n|three" {
		t.Fatalf("deltas %q", deltas)
	}
}

func TestOutputTeeShedsOverlongLineUntilNextNewline(t *testing.T) {
	thread := &Thread{id: "t", host: &Host{}, output: outputRelay{ch: make(chan Update, outputBuffer)}}
	first := "ab\n" + strings.Repeat("x", outputHoldLimit)
	second := "xxx tail\nnext\n"
	tee := thread.teeOutput(&scriptedReads{reads: []runner.Output{
		{Data: []byte(first), Next: int64(len(first))},
		{Data: []byte(second), Next: int64(len(first) + len(second))},
		{Data: []byte("end"), Next: int64(len(first) + len(second) + 3), Exit: &runner.Exit{}},
	}}, "p", "call")
	for range 3 {
		if _, err := tee.Read(t.Context(), "p", 0); err != nil {
			t.Fatal(err)
		}
	}
	deltas := drained(thread)
	if strings.Join(deltas, "|") != "ab\n|next\n|end" || thread.output.dropped.Load() != 1 || cap(tee.pending) > 2*outputHoldLimit+len(second) {
		t.Fatalf("deltas %q, dropped %d, pending cap %d", deltas, thread.output.dropped.Load(), cap(tee.pending))
	}
}

func TestSendOutputDropsWhenRelayIsFull(t *testing.T) {
	thread := &Thread{id: "t", output: outputRelay{ch: make(chan Update, outputBuffer)}}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for range outputBuffer + 10 {
			thread.sendOutput(Update{Kind: UpdateExecOutput, Delta: "x"})
		}
	}()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("sendOutput blocked on a full relay")
	}
	if len(thread.output.ch) != outputBuffer || thread.output.dropped.Load() != 10 {
		t.Fatalf("queued %d, dropped %d", len(thread.output.ch), thread.output.dropped.Load())
	}
}

func TestPublishOutputDropsForSlowSubscriberWithoutClosing(t *testing.T) {
	slow := make(chan Update, 4)
	fast := make(chan Update, 4)
	slow <- Update{}
	slow <- Update{}
	a := &actor{t: &Thread{}, subs: map[chan Update]struct{}{slow: {}, fast: {}}}
	a.publishOutput(Update{Kind: UpdateExecOutput, Delta: "x"})
	if _, open := a.subs[slow]; !open || len(slow) != 2 || len(fast) != 1 || a.t.output.dropped.Load() != 1 {
		t.Fatalf("slow open=%v len=%d fast len=%d dropped=%d", open, len(slow), len(fast), a.t.output.dropped.Load())
	}
}
