package state

import (
	"context"
	"encoding/json/jsontext"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	inboxSize  = 256
	batchLimit = 64
)

type Owner struct {
	port       Port
	invalidate func()
	stateDir   string
	inbox      chan message
	done       chan struct{}
	view       atomic.Pointer[View]
	machine    *machine
	ctx        context.Context
	links      map[client.Link]linkScope
	cancels    map[uint64]context.CancelFunc
	saves      chan restoreImage
	saving     sync.WaitGroup
}

type linkScope struct {
	ctx    context.Context
	cancel context.CancelFunc
}

func New(invalidate func(), stateDir string) *Owner {
	o := &Owner{
		invalidate: invalidate, stateDir: stateDir, inbox: make(chan message, inboxSize), done: make(chan struct{}),
		machine: newMachine(), links: map[client.Link]linkScope{}, cancels: map[uint64]context.CancelFunc{},
		saves: make(chan restoreImage, 1),
	}
	o.view.Store(o.machine.view())
	return o
}

func (o *Owner) View() *View { return o.view.Load() }

func (o *Owner) Submit(intent Intent) { o.post(intent) }

func (o *Owner) PhaseChanged(change client.PhaseChange) { o.post(phaseMsg{change: change}) }

func (o *Owner) Notified(link client.Link, method string, params jsontext.Value) {
	o.post(notifyMsg{link: link, method: method, params: params, at: time.Now()})
}

func requestIDOf(method string, params jsontext.Value) (string, error) {
	header, err := decode[rpc.RequestParams](params)
	if method != rpc.MethodRequest || err != nil || header.Details.RequestID == "" {
		return "", rpc.NativeKind(rpc.ErrorIncompatible)
	}
	return header.Details.RequestID, nil
}

func (o *Owner) Requested(ctx context.Context, link client.Link, method string, params jsontext.Value) (any, error) {
	requestID, err := requestIDOf(method, params)
	if err != nil {
		return nil, err
	}
	waiter := make(chan reply, 1)
	if !o.post(arrivalMsg{link: link, params: params}) {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if !o.post(awaitMsg{link: link, request: requestID, waiter: waiter}) {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	select {
	case r := <-waiter:
		if r.err != nil {
			return nil, r.err
		}
		return r.result, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-o.done:
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
}

func (o *Owner) Run(ctx context.Context, port Port) error {
	o.ctx, o.port = ctx, port
	defer o.shutdown()
	restored, written := loadRestore(o.stateDir)
	o.machine.handle(seedMsg{endpoints: port.Endpoints()})
	o.machine.handle(restored)
	o.machine.dirty = false
	o.saving.Go(func() { o.persist(written) })
	o.publish()
	for {
		select {
		case <-ctx.Done():
			return nil
		case msg := <-o.inbox:
			o.handle(msg)
			o.drain()
			o.publish()
		}
	}
}

func (o *Owner) handle(msg message) {
	if r, ok := msg.(resultMsg); ok {
		if cancel, ok := o.cancels[r.ticket]; ok {
			cancel()
			delete(o.cancels, r.ticket)
		}
	}
	o.machine.handle(msg)
}

func (o *Owner) drain() {
	for range batchLimit {
		select {
		case msg := <-o.inbox:
			o.handle(msg)
		default:
			return
		}
	}
}

func (o *Owner) publish() {
	for _, e := range o.machine.takeEffects() {
		e.run(o)
	}
	if o.machine.dirty {
		o.machine.dirty = false
		o.save(o.machine.restoreImage())
	}
	o.view.Store(o.machine.view())
	o.invalidate()
}

func (o *Owner) save(img restoreImage) {
	select {
	case <-o.saves:
	default:
	}
	o.saves <- img
}

func (o *Owner) persist(written map[string]draftDigest) {
	for img := range o.saves {
		unsaved, err := writeRestore(o.stateDir, img, written)
		o.post(savedMsg{unsaved: unsaved, err: err})
	}
}

func (o *Owner) post(msg message) bool {
	select {
	case o.inbox <- msg:
		return true
	case <-o.done:
		return false
	}
}

func (o *Owner) shutdown() {
	close(o.saves)
	close(o.done)
	o.saving.Wait()
	for _, scope := range o.links {
		scope.cancel()
	}
	for _, cancel := range o.cancels {
		cancel()
	}
	for _, ep := range o.machine.endpoints {
		if ep.attach.detach != nil {
			ep.attach.detach()
		}
	}
}

func (o *Owner) scope(link client.Link) context.Context {
	if s, ok := o.links[link]; ok {
		return s.ctx
	}
	ctx, cancel := context.WithCancel(o.ctx)
	o.links[link] = linkScope{ctx: ctx, cancel: cancel}
	return ctx
}

func (o *Owner) bounded(ticket uint64, link client.Link, deadline time.Duration) context.Context {
	ctx, cancel := context.WithTimeout(o.scope(link), deadline)
	o.cancels[ticket] = cancel
	return ctx
}

func (e callEffect) run(o *Owner) {
	ctx := o.bounded(e.ticket, e.link, e.deadline)
	go func() {
		var raw jsontext.Value
		err := o.port.Call(ctx, e.link, e.method, e.params, &raw)
		o.post(resultMsg{ticket: e.ticket, endpoint: e.link.Endpoint, raw: raw, err: err})
	}()
}

func (e operationEffect) run(o *Owner) {
	ctx := o.bounded(e.ticket, e.link, e.deadline)
	go func() {
		var raw jsontext.Value
		err := o.port.CallOperation(ctx, e.link, e.op, e.params, &raw)
		o.post(resultMsg{ticket: e.ticket, endpoint: e.link.Endpoint, raw: raw, err: err})
	}()
}

func (e readEffect) run(o *Owner) {
	ctx := o.bounded(e.ticket, e.link, e.deadline)
	go func() {
		data, err := readAll(ctx, func(ctx context.Context, offset string) (rpc.ContentChunk, error) {
			var chunk rpc.ContentChunk
			err := o.port.Call(ctx, e.link, e.method, e.params(offset), &chunk)
			return chunk, err
		}, e.summary)
		o.post(resultMsg{ticket: e.ticket, endpoint: e.link.Endpoint, data: data, err: err})
	}()
}

func (e cancelEffect) run(o *Owner) {
	if cancel, ok := o.cancels[e.ticket]; ok {
		cancel()
	}
}

func (e replyEffect) run(*Owner) { e.to <- e.reply }

func (e respondEffect) run(o *Owner) {
	parent := o.scope(e.link)
	go func() {
		ctx, cancel := context.WithTimeout(parent, metadataDeadline)
		defer cancel()
		if err := o.port.Respond(ctx, e.link, e.request, e.result); err != nil {
			o.post(respondedMsg{thread: e.thread, request: e.request, err: err})
		}
	}()
}

func (e connectEffect) run(o *Owner) {
	go func() {
		if err := o.port.Connect(e.endpoint); err != nil {
			o.post(connectFailedMsg{endpoint: e.endpoint, err: err})
		}
	}()
}

func (e resyncEffect) run(o *Owner) {
	go func() {
		err := o.port.Disconnect(e.endpoint)
		if err == nil {
			err = o.port.Connect(e.endpoint)
		}
		if err != nil {
			o.post(connectFailedMsg{endpoint: e.endpoint, err: err})
		}
	}()
}

func (e attachEffect) run(o *Owner) {
	go func() {
		detach, err := o.port.Attach(e.endpoint)
		if !o.post(attachedMsg{endpoint: e.endpoint, detach: detach, err: err}) && detach != nil {
			detach()
		}
	}()
}

func (e detachEffect) run(*Owner) { go e.detach() }

func (e retireEffect) run(o *Owner) {
	if s, ok := o.links[e.link]; ok {
		s.cancel()
		delete(o.links, e.link)
	}
}

func (e timerEffect) run(o *Owner) {
	time.AfterFunc(e.after, func() { o.post(e.fire) })
}
