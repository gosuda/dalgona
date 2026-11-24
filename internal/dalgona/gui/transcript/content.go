package transcript

import (
	"bytes"
	"cmp"
	"errors"
	"strings"
	"sync"
	"unicode/utf8"
	"weak"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	inlineLimit    = 256 << 10
	tailKeep       = 64 << 10
	residentBudget = document.ResidentSourceBudget
	countLimit     = 16 << 20
	readsInFlight  = 4
	settleEpochs   = 2
	fieldSpill     = "outputSpill"
)

var (
	errNoProgress   = errors.New("the daemon returned an empty page before the end of the content; choose Retry")
	errMismatch     = errors.New("the daemon returned content that does not match its reference; choose Retry")
	errSourceBudget = errors.New("the window has no space to hold the complete source")
)

type bodyKey struct {
	thread  state.ThreadKey
	item    state.ItemKey
	content string
	count   bool
}

type body struct {
	key          bodyKey
	ref          rpc.ContentReference
	total        uint64
	start        uint64
	next         uint64
	data         []byte
	lines        int
	last         byte
	err          error
	asked        uint64
	used         uint64
	full         bool
	accessSource bool
}

func (b *body) large() bool { return b.total > inlineLimit }

func (b *body) end() uint64 {
	if b.key.count {
		return min(b.total, countLimit)
	}
	return b.total
}

func (b *body) complete() bool { return b.err == nil && b.next >= b.end() }

func (b *body) serveIncompleteLargeOrFull(full bool) bool {
	if !b.complete() || b.large() || full {
		b.accessSource = true
	}
	return b.accessSource && b.total != 0
}

func (b *body) countPage(data []byte) {
	if len(data) == 0 {
		return
	}
	b.lines += bytes.Count(data, []byte{'\n'})
	b.last = data[len(data)-1]
}

func (b *body) lineTotal() int {
	if b.last == '\n' {
		return b.lines
	}
	return b.lines + 1
}

func (b *body) text() string {
	data := b.data
	if !b.complete() {
		data = validPrefix(data)
	}
	if b.start > 0 {
		if cut := bytes.IndexByte(data, '\n'); cut >= 0 {
			data = data[cut+1:]
		}
	}
	return strings.ToValidUTF8(string(data), "\uFFFD")
}

func validPrefix(data []byte) []byte {
	for cut := len(data); cut > 0 && cut > len(data)-utf8.UTFMax; cut-- {
		if utf8.Valid(data[:cut]) {
			return data[:cut]
		}
	}
	if utf8.Valid(data) {
		return data
	}
	return data[:0]
}

type loader struct {
	bodies          map[bodyKey]*body
	view            weak.Pointer[state.View]
	epoch           uint64
	clock           uint64
	resident        int
	presented       int
	wanted          []*body
	holdMu          sync.Mutex
	holds           map[*byte]sourceHold
	projectionOwner *loader
	projectionHolds []*byte
	cachedSource    int
}

type sourceHold struct {
	bytes  int
	owners int
}

func (l *loader) holdSource(backing *byte, size int) (func(), bool) {
	l.holdMu.Lock()
	defer l.holdMu.Unlock()
	if !l.addSourceHold(backing, size, residentBudget-l.heldSourceBytes()) {
		return nil, false
	}
	return sync.OnceFunc(func() { l.releaseSource(backing) }), true
}

func (l *loader) addSourceHold(backing *byte, size, room int) bool {
	prior := l.holds[backing]
	charge := max(prior.bytes, size)
	if charge-prior.bytes > room {
		return false
	}
	if l.holds == nil {
		l.holds = make(map[*byte]sourceHold)
	}
	l.holds[backing] = sourceHold{bytes: charge, owners: prior.owners + 1}
	return true
}

func (l *loader) heldSourceBytes() int {
	n := 0
	for _, hold := range l.holds {
		n += hold.bytes
	}
	return n
}

func (l *loader) releaseSource(backing *byte) {
	l.holdMu.Lock()
	defer l.holdMu.Unlock()
	hold := l.holds[backing]
	if hold.owners <= 1 {
		delete(l.holds, backing)
		return
	}
	hold.owners--
	l.holds[backing] = hold
}

func (l *loader) projectionRoom() int {
	owner := cmp.Or(l.projectionOwner, l)
	owner.holdMu.Lock()
	defer owner.holdMu.Unlock()
	return owner.availableProjectionSource()
}

func (l *loader) holdProjection(size int) bool {
	owner := cmp.Or(l.projectionOwner, l)
	owner.holdMu.Lock()
	defer owner.holdMu.Unlock()
	key := new(byte)
	if !owner.addSourceHold(key, size, owner.availableProjectionSource()) {
		return false
	}
	l.projectionHolds = append(l.projectionHolds, key)
	return true
}

func (l *loader) availableProjectionSource() int {
	return max(0, residentBudget-l.cachedSource-l.heldSourceBytes())
}

func (l *loader) releaseProjections() {
	owner := cmp.Or(l.projectionOwner, l)
	owner.holdMu.Lock()
	defer owner.holdMu.Unlock()
	l.dropProjections(owner)
}

func (l *loader) dropProjections(owner *loader) {
	for _, key := range l.projectionHolds {
		delete(owner.holds, key)
	}
	clear(l.projectionHolds)
	l.projectionHolds = l.projectionHolds[:0]
}

func newLoader() *loader {
	return &loader{bodies: map[bodyKey]*body{}}
}

func (l *loader) begin(v *state.View) {
	current := weak.Make(v)
	if current != l.view {
		l.view, l.epoch = current, l.epoch+1
	}
	l.clock++
	clear(l.wanted)
	l.wanted = l.wanted[:0]
}

func refNamed(h rpc.ItemHeader, field string) (rpc.ContentReference, bool) {
	for _, ref := range h.Contents {
		if ref.Field == field {
			return ref, true
		}
	}
	return rpc.ContentReference{}, false
}

func (l *loader) want(thread state.ThreadKey, it state.ItemView, field string, full bool) (*body, bool) {
	ref, ok := refNamed(it.Header, field)
	if !ok {
		return nil, false
	}
	return l.wantRef(thread, it.Key, ref, full), true
}

func (l *loader) wantRef(thread state.ThreadKey, item state.ItemKey, ref rpc.ContentReference, full bool) *body {
	return l.track(bodyKey{thread: thread, item: item, content: ref.ContentID}, ref, full)
}

func (l *loader) counter(thread state.ThreadKey, item state.ItemKey, ref rpc.ContentReference) *body {
	return l.track(bodyKey{thread: thread, item: item, content: ref.ContentID, count: true}, ref, false)
}

func (l *loader) track(key bodyKey, ref rpc.ContentReference, full bool) *body {
	b, ok := l.bodies[key]
	if !ok {
		total, err := rpc.ParseDecimal(ref.Total)
		b = &body{key: key, ref: ref, total: total, err: err}
		if total > inlineLimit && !key.count {
			b.start = total - tailKeep
		}
		b.next = b.start
		l.bodies[key] = b
	}
	if full && !key.count {
		l.promote(b)
	}
	if b.used != l.clock {
		b.used = l.clock
		l.wanted = append(l.wanted, b)
	}
	return b
}

func (l *loader) promote(b *body) {
	if b.total > residentBudget {
		b.err = errSourceBudget
		return
	}
	if b.full || b.start == 0 {
		b.full = true
		return
	}
	l.resident -= cap(b.data)
	b.data, b.next, b.asked, b.start, b.full = nil, 0, 0, 0, true
}

func (l *loader) retry(b *body, submit func(state.Intent)) {
	b.err = nil
	b.asked = l.epoch
	submit(state.ReadContent{Thread: b.key.thread, Item: b.key.item, ContentID: b.ref.ContentID, Offset: b.next})
}

func (l *loader) pump(v *state.View, submit func(state.Intent)) bool {
	busy := map[bodyKey]bool{}
	inflight := 0
	waiting := false
	for _, b := range l.wanted {
		if b.complete() || b.err != nil {
			continue
		}
		waiting = true
		tv, ok := v.Thread(b.key.thread)
		if !ok {
			continue
		}
		item := bodyKey{thread: b.key.thread, item: b.key.item}
		if busy[item] {
			continue
		}
		if l.step(tv, b, submit, inflight < readsInFlight) {
			busy[item] = true
			inflight++
		}
	}
	return waiting
}

func (l *loader) step(tv *state.ThreadView, b *body, submit func(state.Intent), admit bool) bool {
	key := state.ContentKey{Item: b.key.item, ContentID: b.ref.ContentID, Offset: b.next}
	page, ok := tv.Content(key)
	recent := b.asked != 0 && l.epoch-b.asked < settleEpochs
	if ok {
		return l.receive(b, key, page, submit)
	}
	if recent || !admit {
		return recent
	}
	b.asked = l.epoch
	submit(state.ReadContent{Thread: b.key.thread, Item: b.key.item, ContentID: b.ref.ContentID, Offset: b.next})
	return true
}

func (l *loader) receive(b *body, key state.ContentKey, page state.ContentPage, submit func(state.Intent)) bool {
	if page.Loading || page.Err != nil && b.asked == l.epoch {
		return true
	}
	submit(state.ReleaseContentPage{Thread: b.key.thread, Key: key})
	if page.Err != nil {
		b.err = page.Err
		return false
	}
	l.consume(b, page)
	return true
}

func (l *loader) consume(b *body, page state.ContentPage) {
	b.asked = 0
	switch {
	case page.Total != b.total || page.Next > b.total || page.Next < b.next:
		b.err = errMismatch
		return
	case page.Next == b.next:
		b.err = errNoProgress
		return
	}
	if b.key.count {
		b.countPage(page.Data)
	} else {
		before := cap(b.data)
		b.data = append(b.data, page.Data...)
		l.resident += cap(b.data) - before
	}
	b.next = page.Next
}

func (l *loader) overBudget() bool { return l.resident+l.presented > residentBudget }

func (l *loader) evict() {
	for l.overBudget() {
		victim := l.unusedVictim()
		if victim == nil {
			victim = l.oldestVictim()
		}
		if victim == nil {
			return
		}
		l.resident -= cap(victim.data)
		victim.data, victim.next, victim.asked = nil, victim.start, 0
		delete(l.bodies, victim.key)
	}
}

func (l *loader) unusedVictim() *body {
	var victim *body
	for _, b := range l.bodies {
		if b.used != l.clock && len(b.data) > 0 && (victim == nil || b.used < victim.used) {
			victim = b
		}
	}
	return victim
}

func (l *loader) oldestVictim() *body {
	var victim *body
	for _, b := range l.bodies {
		if len(b.data) > 0 && (victim == nil || l.evictFirst(b, victim)) {
			victim = b
		}
	}
	return victim
}

func (l *loader) evictFirst(b, v *body) bool {
	if (b.asked == l.epoch) != (v.asked == l.epoch) {
		return b.asked != l.epoch
	}
	return b.used < v.used
}
