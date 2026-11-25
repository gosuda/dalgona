package transcript

import (
	"context"
	"errors"
	"slices"
	"sync"
	"weak"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

var (
	errThreadClosed          = errors.New("the thread closed before its content was read")
	errLiveSourceChanged     = errors.New("the live source changed before it could be read; reopen Full text")
	errRecordedSourceChanged = errors.New("the recorded source changed before it could be read")
)

type liveTail struct {
	backing  weak.Pointer[byte]
	revision string
	field    string
	size     uint64
}

func captureTail(item state.ItemView) liveTail {
	return liveTail{backing: weak.Make(&item.Tail[0]), revision: item.Header.ItemRevision, field: item.Field, size: uint64(len(item.Tail))}
}

func (s liveTail) page(tv *state.ThreadView, key state.ItemKey, offset, limit uint64) ([]byte, error) {
	item, ok := sourceItem(tv, key)
	if !ok || offset >= s.size || !s.matches(item) {
		return nil, errLiveSourceChanged
	}
	return item.Tail[offset : offset+min(limit, s.size-offset)], nil
}

func sourceItem(tv *state.ThreadView, key state.ItemKey) (state.ItemView, bool) {
	i := slices.IndexFunc(tv.Turns, func(t state.TurnView) bool { return t.ID == key.Turn })
	if i < 0 {
		return state.ItemView{}, false
	}
	items := tv.Turns[i].Items
	j := slices.IndexFunc(items, func(item state.ItemView) bool { return item.Key == key })
	if j < 0 {
		return state.ItemView{}, false
	}
	return items[j], true
}

func (s liveTail) matches(item state.ItemView) bool {
	return item.Header.ItemRevision == s.revision && item.Field == s.field &&
		uint64(len(item.Tail)) >= s.size && len(item.Tail) > 0 && weak.Make(&item.Tail[0]) == s.backing
}

type pageRequest struct {
	thread    state.ThreadKey
	item      state.ItemKey
	ref       rpc.ContentReference
	live      liveTail
	link      client.Link
	limit     uint64
	offset    uint64
	asked     uint64
	done      chan pageResult
	abandoned bool
}

type pageResult struct {
	data string
	err  error
}

type bridge struct {
	mu      sync.Mutex
	pending []*pageRequest
}

func (b *bridge) fetch(ctx context.Context, req pageRequest) pageResult {
	req.done = make(chan pageResult, 1)
	b.mu.Lock()
	b.pending = append(b.pending, &req)
	b.mu.Unlock()
	select {
	case r := <-req.done:
		return r
	case <-ctx.Done():
		b.drop(&req)
		return pageResult{err: ctx.Err()}
	}
}

func (b *bridge) drop(req *pageRequest) {
	b.mu.Lock()
	defer b.mu.Unlock()
	for i, r := range b.pending {
		if r == req {
			if req.asked != 0 {
				req.abandoned = true
			} else {
				b.pending = slices.Delete(b.pending, i, i+1)
			}
			return
		}
	}
}

func (b *bridge) abort() {
	b.mu.Lock()
	defer b.mu.Unlock()
	for _, req := range b.pending {
		if !req.abandoned {
			req.abandoned = true
			req.done <- pageResult{err: errThreadClosed}
		}
	}
	b.pending = slices.DeleteFunc(b.pending, func(req *pageRequest) bool { return req.asked == 0 })
}

func (b *bridge) waiting() bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return len(b.pending) > 0
}

func (b *bridge) serve(v *state.View, epoch uint64, submit func(state.Intent)) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if len(b.pending) == 0 {
		return
	}
	req := b.pending[0]
	if result, ok := req.step(v, epoch, submit); ok {
		if !req.abandoned {
			req.done <- result
		}
		b.pending = slices.Delete(b.pending, 0, 1)
	}
}

func (req *pageRequest) step(v *state.View, epoch uint64, submit func(state.Intent)) (pageResult, bool) {
	tv, ok := v.Thread(req.thread)
	if !ok || (req.asked != 0 && tv.Link != req.link) {
		return pageResult{err: errThreadClosed}, true
	}
	if req.live.size > 0 {
		data, err := req.live.page(tv, req.item, req.offset, req.limit)
		return pageResult{data: string(data), err: err}, true
	}
	key := state.ContentKey{Item: req.item, ContentID: req.ref.ContentID, Offset: req.offset}
	page, ok := tv.Content(key)
	if ok {
		return req.consume(tv, key, page, submit)
	}
	if req.recent(epoch) {
		return pageResult{}, false
	}
	if req.abandoned {
		return pageResult{err: errThreadClosed}, true
	}
	if !req.matches(tv) {
		return pageResult{err: errRecordedSourceChanged}, true
	}
	req.asked, req.link = epoch, tv.Link
	submit(state.ReadContent{Thread: req.thread, Item: req.item, ContentID: req.ref.ContentID, Offset: req.offset})
	return pageResult{}, false
}

func (req *pageRequest) consume(tv *state.ThreadView, key state.ContentKey, page state.ContentPage, submit func(state.Intent)) (pageResult, bool) {
	if page.Loading {
		return pageResult{}, false
	}
	submit(state.ReleaseContentPage{Thread: req.thread, Key: key})
	if req.abandoned {
		return pageResult{}, true
	}
	if !req.matches(tv) {
		return pageResult{err: errRecordedSourceChanged}, true
	}
	if page.Err != nil && req.asked == 0 {
		return pageResult{}, false
	}
	data := page.Data[:min(uint64(len(page.Data)), req.limit)]
	return pageResult{data: string(data), err: page.Err}, true
}

func (req *pageRequest) matches(tv *state.ThreadView) bool {
	item, ok := sourceItem(tv, req.item)
	return ok && slices.Contains(item.Header.Contents, req.ref)
}

func (req *pageRequest) recent(epoch uint64) bool {
	return req.asked != 0 && epoch-req.asked < settleEpochs
}
