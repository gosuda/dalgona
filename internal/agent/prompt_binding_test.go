package agent

import (
	"context"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestPromptBindingFreezesSystemAcrossRounds(t *testing.T) {
	var memory atomic.Value
	memory.Store("first decision")
	h := newHarness(t, func(h *harness) {
		h.cfg.MemorySource = func(context.Context, string) (string, error) {
			return memory.Load().(string), nil
		}
	})
	gate := make(chan struct{})
	h.model.push(response{gate: gate, body: sseCalls(call{id: "read-one", name: "read", args: `{"path":"missing-file"}`})}, text("done"), text("next"), text("cleared"))
	thread := h.start()
	h.submit(thread, "first turn")
	select {
	case <-h.model.arrived:
	case <-t.Context().Done():
		t.Fatal(t.Context().Err())
	}
	memory.Store("second decision")
	h.host.RefreshPrompts(h.cwd)
	close(gate)
	h.waitIdle(thread)
	first, ok := h.model.request(0)["instructions"].(string)
	if !ok || !strings.HasSuffix(first, "\n\n## Project memory\n\nfirst decision") {
		t.Fatalf("first request lacks accepted memory: %q", first)
	}
	if next := h.model.request(1)["instructions"]; next != first {
		t.Fatalf("active turn changed its System: %v", next)
	}
	h.submit(thread, "next turn")
	h.waitIdle(thread)
	base := strings.TrimSuffix(first, "\n\n## Project memory\n\nfirst decision")
	if got := h.model.request(2)["instructions"]; got != base+"\n\n## Project memory\n\nsecond decision" {
		t.Fatalf("next turn did not preserve base and replace suffix: %v", got)
	}
	memory.Store("")
	h.host.RefreshPrompts(h.cwd)
	h.submit(thread, "clear turn")
	h.waitIdle(thread)
	if got := h.model.request(3)["instructions"]; got != base {
		t.Fatalf("explicit clear changed base: %v", got)
	}
}

func TestDalNeverReadsProjectMemory(t *testing.T) {
	var reads atomic.Int32
	h := newHarness(t, func(h *harness) {
		h.cfg.Product = config.ProductDal
		h.cfg.Config.Product = config.ProductDal
		h.cfg.MemorySource = func(context.Context, string) (string, error) {
			reads.Add(1)
			return "must not enter request", nil
		}
	})
	thread := h.start()
	h.host.RefreshPrompts(h.cwd)
	h.submit(thread, "hello")
	h.waitIdle(thread)
	if reads.Load() != 0 {
		t.Fatal("dal consulted the memory source")
	}
	if system, _ := h.model.request(0)["instructions"].(string); strings.Contains(system, "## Project memory") {
		t.Fatal("dal request includes project memory")
	}
}
