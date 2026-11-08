package agent

import (
	"bytes"
	"context"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/runner"
)

const (
	outputBuffer    = 256
	outputHoldLimit = 64 << 10
)

type outputRelay struct {
	ch      chan Update
	dropped atomic.Uint64
}

func (t *Thread) sendOutput(update Update) {
	update.Thread = t.id
	select {
	case t.output.ch <- update:
	default:
		t.output.dropped.Add(1)
	}
}

func (a *actor) drainOutput() {
	for {
		select {
		case update := <-a.t.output.ch:
			a.publishOutput(update)
		default:
			return
		}
	}
}

func (a *actor) publishOutput(update Update) {
	for ch := range a.subs {
		if !offerOutput(ch, update) {
			a.t.output.dropped.Add(1)
		}
	}
}

func offerOutput(ch chan Update, update Update) bool {
	if len(ch) >= cap(ch)/2 {
		return false
	}
	select {
	case ch <- update:
		return true
	default:
		return false
	}
}

type outputTee struct {
	runner.Runner
	thread   *Thread
	process  string
	callID   string
	seen     int64
	pending  []byte
	skipping bool
}

func (t *Thread) teeOutput(run runner.Runner, process, callID string) *outputTee {
	return &outputTee{Runner: run, thread: t, process: process, callID: callID}
}

func (o *outputTee) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	out, err := o.Runner.Read(ctx, id, offset)
	if err == nil && id == o.process {
		o.take(out)
	}
	return out, err
}

func (o *outputTee) take(out runner.Output) {
	if out.Next > o.seen {
		o.accept(out.Data[max(0, int64(len(out.Data))-(out.Next-o.seen)):])
		o.seen = out.Next
	}
	if out.Exit != nil {
		o.flush()
		return
	}
	if cut := bytes.LastIndexAny(o.pending, "\n\r"); cut >= 0 {
		o.emit(cut + 1)
	}
	if len(o.pending) >= outputHoldLimit {
		o.pending = o.pending[:0]
		o.skipping = true
		o.thread.output.dropped.Add(1)
	}
}

func (o *outputTee) accept(fresh []byte) {
	if o.skipping {
		end := bytes.IndexAny(fresh, "\n\r")
		if end < 0 {
			return
		}
		fresh, o.skipping = fresh[end+1:], false
	}
	o.pending = append(o.pending, fresh...)
}

func (o *outputTee) flush() {
	if len(o.pending) > 0 {
		o.emit(len(o.pending))
	}
}

func (o *outputTee) emit(n int) {
	text := o.thread.host.mask(string(o.pending[:n]))
	o.pending = o.pending[:copy(o.pending, o.pending[n:])]
	if text != "" {
		o.thread.sendOutput(Update{Kind: UpdateExecOutput, CallID: o.callID, Delta: text})
	}
}
