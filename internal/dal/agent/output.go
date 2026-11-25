package agent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"hash"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/runner"
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
	a.rememberDelta(update)
	for ch := range a.subs {
		if _, seeded := a.seededSubs[ch]; seeded {
			a.sendUpdate(ch, update)
			continue
		}
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
	thread      *Thread
	process     string
	callID      string
	seen        int64
	pending     []byte
	skipping    bool
	mask        *auth.LiveMask
	deferred    bool
	maskedBytes int64
	maskedHash  hash.Hash
}

func (t *Thread) teeOutput(run runner.Runner, process, callID string) *outputTee {
	return &outputTee{Runner: run, thread: t, process: process, callID: callID}
}

func (o *outputTee) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	out, err := o.Runner.Read(ctx, id, offset)
	if err == nil && id == o.process {
		o.takeRead(out, offset, true)
	}
	return out, err
}

func (o *outputTee) ReadPage(ctx context.Context, id string, offset, limit int64) (runner.Output, error) {
	out, err := runner.ReadProcessPage(ctx, o.Runner, id, offset, limit)
	if err == nil && id == o.process {
		o.takeRead(out, offset, int64(len(out.Data)) < limit)
	}
	return out, err
}

func (o *outputTee) takeRead(out runner.Output, offset int64, complete bool) {
	if store := o.thread.host.cfg.Auth; store != nil {
		o.takeMasked(out, offset, store)
		return
	}
	o.takeUnmasked(out, complete)
}

func (o *outputTee) takeUnmasked(out runner.Output, complete bool) {
	if out.Next > o.seen {
		o.accept(out.Data[max(0, int64(len(out.Data))-(out.Next-o.seen)):])
		o.seen = out.Next
	}
	if out.Exit != nil && complete {
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
	text := string(o.pending[:n])
	o.pending = o.pending[:copy(o.pending, o.pending[n:])]
	if text != "" {
		o.thread.sendOutput(Update{Kind: UpdateExecOutput, CallID: o.callID, Delta: text})
	}
}

func (o *outputTee) takeMasked(out runner.Output, offset int64, store *auth.Store) {
	if o.deferred {
		return
	}
	if !o.contiguous(out, offset) {
		o.deferMasked()
		return
	}
	if out.Next <= o.seen {
		return
	}
	if !o.prepareLiveMask(store) {
		return
	}
	o.acceptMasked(out.Data[o.seen-offset:])
}

func (o *outputTee) contiguous(out runner.Output, offset int64) bool {
	if offset < 0 || offset > o.seen || out.Next < offset || out.Next-offset != int64(len(out.Data)) {
		return false
	}
	base := o.seen - int64(len(o.pending))
	start, end := max(offset, base), min(out.Next, o.seen)
	return start >= end || bytes.Equal(out.Data[start-offset:end-offset], o.pending[start-base:end-base])
}

func (o *outputTee) prepareLiveMask(store *auth.Store) bool {
	if o.mask != nil {
		return true
	}
	mask, err := store.NewLiveMask()
	if err != nil {
		o.deferMasked()
		return false
	}
	o.mask = mask
	o.pending = make([]byte, 0, outputHoldLimit)
	return true
}

func (o *outputTee) acceptMasked(data []byte) {
	for len(data) != 0 && !o.deferred {
		n := min(len(data), outputHoldLimit-len(o.pending))
		o.pending = append(o.pending, data[:n]...)
		o.seen += int64(n)
		data = data[n:]
		o.emitVerifiedPrefix()
		if len(o.pending) == outputHoldLimit {
			o.deferMasked()
		}
	}
}

func (o *outputTee) emitVerifiedPrefix() {
	base := o.seen - int64(len(o.pending))
	safe := o.mask.SafeBytes(o.pending, base)
	cut := bytes.LastIndexAny(o.pending[:safe], "\n\r") + 1
	if cut == 0 && len(o.pending) == outputHoldLimit {
		cut = safe
	}
	if cut == 0 {
		return
	}
	masked, consumed, err := o.mask.MaskPrefix(o.pending, cut, outputHoldLimit)
	if err != nil {
		o.deferMasked()
		return
	}
	n := copy(o.pending, o.pending[consumed:])
	clear(o.pending[n:])
	o.pending = o.pending[:n]
	o.publishVerified(masked)
}

func (o *outputTee) deferMasked() {
	o.deferred = true
	clear(o.pending)
	o.pending = nil
	o.mask = nil
}

func (o *outputTee) publishVerified(text string) {
	if text == "" {
		return
	}
	if o.maskedHash == nil {
		o.maskedHash = sha256.New()
	}
	_, _ = o.maskedHash.Write([]byte(text))
	o.maskedBytes += int64(len(text))
	o.thread.sendOutput(Update{Kind: UpdateExecOutput, CallID: o.callID, Delta: text})
}

func (o *outputTee) deliverMaskedCompletion(text string) {
	defer o.deferMasked()
	if o.maskedBytes >= int64(len(text)) {
		return
	}
	if o.maskedHash != nil {
		var live [sha256.Size]byte
		o.maskedHash.Sum(live[:0])
		if sha256.Sum256([]byte(text[:o.maskedBytes])) != live {
			return
		}
	}
	o.thread.sendOutput(Update{Kind: UpdateExecOutput, CallID: o.callID, Delta: text[o.maskedBytes:]})
}
