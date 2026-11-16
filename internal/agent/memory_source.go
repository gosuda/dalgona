package agent

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"maps"
	"path/filepath"
	"slices"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
)

const memoryReadTimeout = 5 * time.Second

var (
	errMemoryTimeout = errors.New("memory: source read timed out")
	errMemoryBusy    = errors.New("memory: source read is still running")
)

type refreshLedger struct {
	mu      sync.Mutex
	posted  map[string]uint64
	served  map[string]uint64
	primed  map[string]bool
	reading map[string]bool
}

func (l *refreshLedger) post(id string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.posted == nil {
		l.posted = make(map[string]uint64)
	}
	l.posted[id]++
}

func (l *refreshLedger) due(id string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return !l.primed[id] || l.posted[id] > l.served[id]
}

func (l *refreshLedger) requested(id string) uint64 {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.posted[id]
}

func (l *refreshLedger) serve(id string, upTo uint64) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.served == nil {
		l.served = make(map[string]uint64)
		l.primed = make(map[string]bool)
	}
	l.served[id] = max(l.served[id], upTo)
	l.primed[id] = true
}

type memoryRead struct {
	content   string
	err       error
	requested uint64
}

func (h *Host) memoryEnabled() bool {
	return h.cfg.Product != config.ProductDal && h.cfg.MemorySource != nil
}

func (h *Host) RefreshPrompts(cwd string) {
	if !h.memoryEnabled() {
		return
	}
	want, err := canonicalDir(cwd)
	if err != nil {
		h.log.Warn("agent: memory refresh needs a resolvable workspace", "error", h.maskedText(err.Error()))
		return
	}
	for _, thread := range h.loadedThreads() {
		have, err := canonicalDir(thread.created.Cwd)
		if err != nil {
			h.log.Warn("agent: memory refresh skipped a thread", "thread", thread.id, "error", h.maskedText(err.Error()))
			continue
		}
		if have == want {
			h.refreshes.post(thread.id)
		}
	}
}

func (h *Host) loadedThreads() []*Thread {
	h.mu.Lock()
	defer h.mu.Unlock()
	return slices.Collect(maps.Values(h.threads))
}

func (h *Host) maskedText(text string) string {
	masked, err := h.mask(text)
	if err != nil {
		return "(withheld because masking failed)"
	}
	return masked
}

func canonicalDir(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", fmt.Errorf("agent: resolve %s: %w", path, err)
	}
	resolved, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return "", fmt.Errorf("agent: resolve %s: %w", abs, err)
	}
	return resolved, nil
}

func (h *Host) promptWindow(modelID string) int {
	provider, id, err := model.ParseModelID(modelID)
	if err != nil {
		return model.ContextWindow(modelID, 0, h.contextOverrides())
	}
	preset, _ := model.Capabilities(provider, id)
	return model.ContextWindow(modelID, preset.Context, h.contextOverrides())
}

func (w *worker) readMemory(ctx context.Context) memoryRead {
	host := w.t.host
	read := memoryRead{requested: host.refreshes.requested(w.t.id)}
	if !host.refreshes.beginRead(w.t.id) {
		read.err = errMemoryBusy
		return read
	}
	cwd, err := canonicalDir(w.t.created.Cwd)
	if err != nil {
		host.refreshes.endRead(w.t.id)
		read.err = err
		return read
	}
	timeout := cmp.Or(host.memoryTimeout, memoryReadTimeout)
	readCtx, cancel := context.WithTimeoutCause(ctx, timeout, errMemoryTimeout)
	defer cancel()
	done := make(chan memoryRead, 1)
	w.t.workers.Go(func() {
		defer host.refreshes.endRead(w.t.id)
		content, failure := host.cfg.MemorySource(readCtx, cwd)
		done <- memoryRead{content: content, err: failure, requested: read.requested}
	})
	select {
	case result := <-done:
		if readCtx.Err() == nil {
			return result
		}
	case <-readCtx.Done():
	}
	read.err = context.Cause(readCtx)
	if errors.Is(read.err, errMemoryTimeout) {
		read.err = fmt.Errorf("%w after %s", errMemoryTimeout, timeout)
	}
	return read
}

func (l *refreshLedger) beginRead(id string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.reading[id] {
		return false
	}
	if l.reading == nil {
		l.reading = make(map[string]bool)
	}
	l.reading[id] = true
	return true
}

func (l *refreshLedger) endRead(id string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	delete(l.reading, id)
}
