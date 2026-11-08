package advisor

import (
	"errors"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
)

func startThread(t *testing.T, h *harness, spec agent.ThreadSpec) *agent.Thread {
	t.Helper()
	thread, err := h.host.Start(t.Context(), spec)
	if err != nil {
		t.Fatal(err)
	}
	return thread
}

func attachConcurrently(t *testing.T, h *harness, registry *Registry, threads []*agent.Thread, callers int) []*Runtime {
	t.Helper()
	got := make([]*Runtime, callers*len(threads))
	var wg sync.WaitGroup
	for i := range got {
		wg.Go(func() {
			runtime, err := registry.Attach(t.Context(), h.host, threads[i%len(threads)])
			if err != nil {
				t.Error(err)
			}
			got[i] = runtime
		})
	}
	wg.Wait()
	return got
}

func assertOneRuntimePerThread(t *testing.T, registry *Registry, threads []*agent.Thread, got []*Runtime) {
	t.Helper()
	for i, runtime := range got {
		want := registry.Get(threads[i%len(threads)].ID())
		if runtime == nil || runtime != want {
			t.Fatalf("caller %d got runtime %p, registry holds %p", i, runtime, want)
		}
	}
	if got[0] == got[1] {
		t.Fatal("two threads share one runtime")
	}
}

func assertStopped(t *testing.T, runtime *Runtime, what string) {
	t.Helper()
	select {
	case <-runtime.done:
	default:
		t.Fatalf("%s left a runtime running", what)
	}
}

func assertClosedRegistry(t *testing.T, h *harness, registry *Registry, thread *agent.Thread) {
	t.Helper()
	if runtime := registry.Get(thread.ID()); runtime != nil {
		t.Fatalf("closed registry still returns %p", runtime)
	}
	if _, err := registry.Attach(t.Context(), h.host, thread); !errors.Is(err, ErrRegistryClosed) {
		t.Fatalf("attach after close: %v", err)
	}
	if err := registry.Close(); err != nil {
		t.Fatalf("second close: %v", err)
	}
}

func TestRegistryAttachesOncePerThreadAndClosesEveryRuntime(t *testing.T) {
	t.Parallel()
	h := newHarness(t, nil)
	registry := NewRegistry()
	threads := []*agent.Thread{startThread(t, h, agent.ThreadSpec{}), startThread(t, h, agent.ThreadSpec{})}
	got := attachConcurrently(t, h, registry, threads, 8)
	assertOneRuntimePerThread(t, registry, threads, got)
	if err := registry.Close(); err != nil {
		t.Fatal(err)
	}
	for _, runtime := range got[:len(threads)] {
		assertStopped(t, runtime, "registry close")
	}
	assertClosedRegistry(t, h, registry, threads[0])
}

func reviewOnce(t *testing.T, h *harness, primary *agent.Thread, reviews int) {
	t.Helper()
	h.submit(primary, "work")
	h.model.waitRequests(roleAdvisor, reviews)
	h.waitFor("the advisor review to end", func() bool {
		return len(turnEnds(journalEvents(t, h.store, childOf(t, h.host, primary.ID())))) == reviews
	})
	h.waitIdle(primary)
}

func reopen(t *testing.T, h *harness, registry *Registry, child *agent.Thread) *agent.Thread {
	t.Helper()
	if err := child.Close(); err != nil {
		t.Fatal(err)
	}
	if runtime := registry.Get(child.ID()); runtime != nil {
		t.Fatalf("registry returns runtime %p of a closed thread", runtime)
	}
	reopened, err := h.host.Resume(t.Context(), child.ID())
	if err != nil {
		t.Fatal(err)
	}
	return reopened
}

func TestRegistryReattachesAReopenedChildWithItsOptIn(t *testing.T) {
	t.Parallel()
	h := newHarness(t, nil)
	registry := NewRegistry()
	t.Cleanup(func() { _ = registry.Close() })
	parent := startThread(t, h, agent.ThreadSpec{})
	child := startThread(t, h, agent.ThreadSpec{Parent: parent.ID()})
	first, err := registry.AttachChild(t.Context(), h.host, child)
	if err != nil {
		t.Fatal(err)
	}
	assertState(t, first, StateIdle)
	reviewOnce(t, h, child, 1)
	reopened := reopen(t, h, registry, child)
	second, err := registry.Attach(t.Context(), h.host, reopened)
	if err != nil {
		t.Fatal(err)
	}
	if second == first || second.primary != reopened || registry.Get(child.ID()) != second {
		t.Fatalf("reattach kept runtime %p on primary %p, want a new runtime on %p", second, second.primary, reopened)
	}
	assertStopped(t, first, "reattach")
	assertEnabled(t, second)
	reviewOnce(t, h, reopened, 2)
}

func assertState(t *testing.T, runtime *Runtime, want State) {
	t.Helper()
	states, err := runtime.Status()
	if err != nil || len(states) != 1 || states[0].State != want {
		t.Fatalf("advisor states = %+v, err %v, want one %s", states, err, want)
	}
}

func assertEnabled(t *testing.T, runtime *Runtime) {
	t.Helper()
	states, err := runtime.Status()
	if err != nil || len(states) != 1 || states[0].State == StateDisabled {
		t.Fatalf("advisor states = %+v, err %v, want one enabled advisor", states, err)
	}
}
