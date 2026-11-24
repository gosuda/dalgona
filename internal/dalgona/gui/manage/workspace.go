//go:build gui

package manage

import (
	"slices"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type workspaceKey struct {
	endpoint client.EndpointID
	thread   string
	rights   string
}

type lease struct {
	key   workspaceKey
	grant workspaceGrant
	link  client.Link
}

type workspaceRecord struct {
	link      client.Link
	grant     workspaceGrant
	opened    bool
	interests int
	owned     int
	closing   bool
	waiters   []func(lease, error)
}

type workspaces struct {
	hub     *Hub
	mu      sync.Mutex
	records map[workspaceKey]*workspaceRecord
}

func newWorkspaces(h *Hub) *workspaces {
	return &workspaces{hub: h, records: map[workspaceKey]*workspaceRecord{}}
}

func rightsKey(rights []string) string {
	sorted := slices.Clone(rights)
	slices.Sort(sorted)
	return strings.Join(sorted, "+")
}

func (w *workspaces) acquire(b binding, thread string, rights []string, done func(lease, error)) {
	key := workspaceKey{endpoint: b.endpoint, thread: thread, rights: rightsKey(rights)}
	w.mu.Lock()
	rec := w.records[key]
	if rec != nil && (rec.closing || rec.link != b.link) {
		rec = nil
	}
	if rec != nil && rec.opened && !rec.closing {
		rec.interests++
		l := lease{key: key, grant: rec.grant, link: rec.link}
		w.mu.Unlock()
		done(l, nil)
		return
	}
	if rec != nil {
		rec.waiters = append(rec.waiters, done)
		w.mu.Unlock()
		return
	}
	rec = &workspaceRecord{link: b.link, waiters: []func(lease, error){done}}
	w.records[key] = rec
	w.mu.Unlock()
	go w.open(b, key, rights, rec)
}

func (w *workspaces) open(b binding, key workspaceKey, rights []string, rec *workspaceRecord) {
	var grant workspaceGrant
	params := workspaceOpenParams{ThreadID: key.thread, Root: "cwd", Rights: rights, OperationID: rpc.NewOperationID()}
	err := w.hub.call(b, decisionDeadline, methodWorkspaceOpen, params, &grant)
	w.mu.Lock()
	waiters := rec.waiters
	rec.waiters = nil
	switch {
	case err == nil:
		rec.grant, rec.opened, rec.interests = grant, true, len(waiters)
	case w.records[key] == rec:
		delete(w.records, key)
	}
	w.mu.Unlock()
	l := lease{key: key, grant: grant, link: rec.link}
	for _, done := range waiters {
		done(l, err)
	}
}

func (w *workspaces) own(l lease) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	rec := w.records[l.key]
	if rec == nil || rec.grant.WorkspaceID != l.grant.WorkspaceID {
		return false
	}
	rec.owned++
	return true
}

func (w *workspaces) disown(l lease) {
	w.mu.Lock()
	rec := w.records[l.key]
	if rec == nil || rec.grant.WorkspaceID != l.grant.WorkspaceID {
		w.mu.Unlock()
		return
	}
	rec.owned--
	closeNow := w.closableLocked(rec)
	w.mu.Unlock()
	if closeNow {
		go w.close(l.key, rec)
	}
}

func (w *workspaces) release(l lease) {
	w.mu.Lock()
	rec := w.records[l.key]
	if rec == nil || rec.grant.WorkspaceID != l.grant.WorkspaceID || rec.interests == 0 {
		w.mu.Unlock()
		return
	}
	rec.interests--
	closeNow := w.closableLocked(rec)
	w.mu.Unlock()
	if closeNow {
		go w.close(l.key, rec)
	}
}

func (w *workspaces) invalidate(l lease) {
	w.mu.Lock()
	defer w.mu.Unlock()
	rec := w.records[l.key]
	if rec == nil || rec.grant.WorkspaceID != l.grant.WorkspaceID {
		return
	}
	delete(w.records, l.key)
}

func (w *workspaces) closableLocked(rec *workspaceRecord) bool {
	if rec.interests > 0 || rec.owned > 0 || rec.closing || !rec.opened {
		return false
	}
	rec.closing = true
	return true
}

func (w *workspaces) close(key workspaceKey, rec *workspaceRecord) {
	var closed workspaceClosed
	err := w.hub.deps.Session.Call(w.hub.ctx, rec.link, methodWorkspaceClose, workspaceOnly{WorkspaceID: rec.grant.WorkspaceID}, &closed)
	w.mu.Lock()
	defer w.mu.Unlock()
	if kindOf(err) == rpc.ErrorBusy {
		rec.closing = false
		return
	}
	if w.records[key] == rec {
		delete(w.records, key)
	}
}
