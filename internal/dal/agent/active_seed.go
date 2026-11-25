package agent

import (
	"context"
	"errors"
	"fmt"
	"iter"
	"os"
	"slices"
	"sync"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	seedMemoryLimit  = 256 << 10
	seedChunkBytes   = 64 << 10
	seedItemLimit    = 4096
	threadSpillLimit = 256 << 20
	daemonSpillLimit = 1 << 30
)

var ErrActiveSpillLimit = errors.New("active projection storage limit exceeded")

var projectionSpill = struct {
	sync.Mutex
	threads map[string]int64
	total   int64
}{threads: map[string]int64{}}

func ReserveActiveSpill(thread string, size int64) bool {
	projectionSpill.Lock()
	defer projectionSpill.Unlock()
	if size > threadSpillLimit-projectionSpill.threads[thread] || size > daemonSpillLimit-projectionSpill.total {
		return false
	}
	projectionSpill.threads[thread] += size
	projectionSpill.total += size
	return true
}

func ReleaseActiveSpill(thread string, size int64) {
	projectionSpill.Lock()
	defer projectionSpill.Unlock()
	projectionSpill.threads[thread] -= size
	if projectionSpill.threads[thread] == 0 {
		delete(projectionSpill.threads, thread)
	}
	projectionSpill.total -= size
}

type SeededSubscription struct {
	Updates    <-chan Update
	Head       uint64
	Seed       iter.Seq2[Update, error]
	Release    func()
	Overflowed func() bool
}

func (t *Thread) SubscribeSeeded(ctx context.Context) (SeededSubscription, error) {
	reply := make(chan SeededSubscription)
	select {
	case t.inbox <- func(a *actor) { a.offerSeededSubscription(ctx, reply) }:
	case <-t.quit:
		return SeededSubscription{}, ErrClosed
	case <-ctx.Done():
		return SeededSubscription{}, ctx.Err()
	}
	select {
	case subscription := <-reply:
		return subscription, nil
	case <-t.stopped:
		return SeededSubscription{}, ErrClosed
	case <-ctx.Done():
		return SeededSubscription{}, ctx.Err()
	}
}

func (a *actor) offerSeededSubscription(ctx context.Context, reply chan<- SeededSubscription) {
	ch := make(chan Update, subscriberBuffer)
	if a.seededSubs == nil {
		a.seededSubs = make(map[chan Update]*atomic.Bool)
	}
	overflowed := &atomic.Bool{}
	a.seededSubs[ch] = overflowed
	a.subscribe(ch)
	prefixes := make([]deltaPrefix, 0, len(a.activeDeltas.items))
	for _, body := range a.activeDeltas.items {
		prefixes = append(prefixes, body.freeze())
	}
	release := sync.OnceFunc(func() {
		for _, prefix := range prefixes {
			prefix.body.unpin()
		}
	})
	fault := a.activeDeltas.fault
	sub := SeededSubscription{Updates: ch, Head: a.head(), Seed: seedUpdates(prefixes, fault), Release: release, Overflowed: overflowed.Load}
	select {
	case reply <- sub:
	case <-ctx.Done():
		release()
		a.unsubscribe(ch)
	}
}

type activeDeltas struct {
	items []*deltaBody
	fault error
}

func (a *actor) rememberDelta(update Update) {
	if a.state.TurnID == "" || update.Delta == "" {
		return
	}
	key := deltaKey{kind: update.Kind, item: update.ItemID}
	if update.Kind == UpdateExecOutput {
		if _, active := a.state.InFlight[update.CallID]; !active {
			return
		}
		key.item = update.CallID
	}
	if key.item == "" {
		return
	}
	a.activeDeltas.append(a.t.id, key, update.Delta)
}

func (a *activeDeltas) update(eventValue event.Event) {
	switch value := eventValue.(type) {
	case event.AssistantMessage:
		a.drop(func(key deltaKey) bool { return key.kind != UpdateExecOutput && key.item == value.ItemID })
	case event.ToolCompleted:
		a.drop(func(key deltaKey) bool { return key.kind == UpdateExecOutput && key.item == value.CallID })
	case event.ModelRetry:
		a.drop(func(key deltaKey) bool { return key.kind != UpdateExecOutput })
	case event.TurnStarted, event.TurnCompleted:
		a.clear()
	}
}

func (a *activeDeltas) clear() {
	for _, body := range a.items {
		body.retire()
	}
	a.items = nil
	a.fault = nil
}

func (a *activeDeltas) drop(matches func(deltaKey) bool) {
	a.items = slices.DeleteFunc(a.items, func(body *deltaBody) bool {
		if !matches(body.key) {
			return false
		}
		body.retire()
		return true
	})
}

func (a *activeDeltas) append(thread string, key deltaKey, text string) {
	for _, body := range a.items {
		if body.key == key {
			body.append(text)
			return
		}
	}
	if len(a.items) >= seedItemLimit {
		a.fault = ErrActiveSpillLimit
		return
	}
	body := &deltaBody{thread: thread, key: key}
	body.append(text)
	a.items = append(a.items, body)
}

type deltaKey struct {
	kind UpdateKind
	item string
}

type deltaBody struct {
	mu      sync.Mutex
	thread  string
	key     deltaKey
	memory  []byte
	spill   *os.File
	stored  int64
	pins    int
	retired bool
	fault   error
}

type deltaPrefix struct {
	body   *deltaBody
	length int64
	fault  error
}

func (b *deltaBody) append(text string) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.fault != nil {
		return
	}
	room := min(len(text), seedMemoryLimit-len(b.memory))
	b.memory = append(b.memory, text[:room]...)
	if room < len(text) {
		b.fault = b.appendSpill(text[room:])
	}
}

func (b *deltaBody) appendSpill(text string) error {
	cost := int64(len(text))
	if !ReserveActiveSpill(b.thread, cost) {
		return ErrActiveSpillLimit
	}
	if b.spill == nil {
		file, err := os.CreateTemp("", "dal-delta-seed-*")
		if err != nil {
			ReleaseActiveSpill(b.thread, cost)
			return fmt.Errorf("create active seed spill: %w", err)
		}
		b.spill = file
	}
	n, err := b.spill.WriteString(text)
	b.stored += int64(n)
	if err != nil {
		ReleaseActiveSpill(b.thread, cost-int64(n))
		return fmt.Errorf("write active seed spill: %w", err)
	}
	return nil
}

func (b *deltaBody) freeze() deltaPrefix {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.pins++
	return deltaPrefix{body: b, length: int64(len(b.memory)) + b.stored, fault: b.fault}
}

func (b *deltaBody) retire() {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.retired = true
	b.releaseLocked()
}

func (b *deltaBody) unpin() {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.pins--
	b.releaseLocked()
}

func (b *deltaBody) releaseLocked() {
	if !b.retired || b.pins != 0 || b.spill == nil {
		return
	}
	name := b.spill.Name()
	_ = b.spill.Close()
	_ = os.Remove(name)
	b.spill = nil
	ReleaseActiveSpill(b.thread, b.stored)
}

func seedUpdates(prefixes []deltaPrefix, fault error) iter.Seq2[Update, error] {
	return func(yield func(Update, error) bool) {
		if fault != nil {
			yield(Update{}, fault)
			return
		}
		for _, prefix := range prefixes {
			if !prefix.yield(yield) {
				return
			}
		}
	}
}

func (p deltaPrefix) yield(yield func(Update, error) bool) bool {
	if p.fault != nil {
		yield(Update{}, p.fault)
		return false
	}
	update := Update{Kind: p.body.key.kind, Thread: p.body.thread, ItemID: p.body.key.item}
	if update.Kind == UpdateExecOutput {
		update.CallID, update.ItemID = update.ItemID, ""
	}
	buffer := make([]byte, min(p.length, seedChunkBytes))
	for offset := int64(0); offset < p.length; offset += int64(len(buffer)) {
		part := buffer[:min(int64(len(buffer)), p.length-offset)]
		err := p.body.read(part, offset)
		update.Delta = string(part)
		if !yield(update, err) || err != nil {
			return false
		}
	}
	return true
}

func (b *deltaBody) read(part []byte, offset int64) error {
	b.mu.Lock()
	defer b.mu.Unlock()
	memoryBytes := int64(len(b.memory))
	if offset < memoryBytes {
		n := copy(part, b.memory[offset:])
		part = part[n:]
		offset += int64(n)
	}
	if len(part) == 0 {
		return nil
	}
	_, err := b.spill.ReadAt(part, offset-memoryBytes)
	return err
}
