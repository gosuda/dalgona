package rpc

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"sync"
)

var ErrRequestCancelled = errors.New("rpc: the client cancelled the request")

type cancelRequestParams struct {
	ID jsontext.Value `json:"id"`
}

type serverReq struct {
	conn      *Conn
	key       string
	mu        sync.Mutex
	requested bool
	cancel    context.CancelCauseFunc
}

type serverReqKey struct{}

func (c *Conn) trackServerRequest(id jsontext.Value) (context.Context, *serverReq) {
	req := &serverReq{conn: c}
	ctx := context.WithValue(c.ctx, serverReqKey{}, req)
	if len(id) == 0 {
		return ctx, req
	}
	req.key = requestKey(id)
	c.serverReqMu.Lock()
	defer c.serverReqMu.Unlock()
	if c.serverReqs == nil {
		c.serverReqs = make(map[string]*serverReq)
	}
	c.serverReqs[req.key] = req
	return ctx, req
}

func (r *serverReq) finish() {
	if r.key == "" {
		return
	}
	r.conn.serverReqMu.Lock()
	defer r.conn.serverReqMu.Unlock()
	if r.conn.serverReqs[r.key] == r {
		delete(r.conn.serverReqs, r.key)
	}
}

func (c *Conn) cancelServerRequest(params jsontext.Value) {
	var p cancelRequestParams
	if json.Unmarshal(params, &p) != nil || len(p.ID) == 0 {
		return
	}
	c.serverReqMu.Lock()
	req := c.serverReqs[requestKey(p.ID)]
	c.serverReqMu.Unlock()
	if req == nil {
		return
	}
	req.mu.Lock()
	defer req.mu.Unlock()
	req.requested = true
	if req.cancel != nil {
		req.cancel(ErrRequestCancelled)
	}
}

func RequestScope(ctx context.Context) (context.Context, context.CancelFunc) {
	scoped, cancel := context.WithCancelCause(ctx)
	req, ok := ctx.Value(serverReqKey{}).(*serverReq)
	if !ok {
		return scoped, func() { cancel(context.Canceled) }
	}
	req.mu.Lock()
	req.cancel = cancel
	if req.requested {
		cancel(ErrRequestCancelled)
	}
	req.mu.Unlock()
	return scoped, func() {
		req.mu.Lock()
		req.cancel = nil
		req.mu.Unlock()
		cancel(context.Canceled)
	}
}

func requestKey(id jsontext.Value) string {
	canonical := jsontext.Value(bytes.Clone(id))
	if err := canonical.Canonicalize(); err != nil {
		return string(id)
	}
	return string(canonical)
}

func (c *Conn) EnableRequestCancellation() { c.cancelRemote.Store(true) }
