package agent

import (
	"encoding/json/v2"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/journal"
)

func firstUserText(body []byte) string {
	var decoded map[string]any
	if err := json.Unmarshal(body, &decoded); err != nil {
		return ""
	}
	texts := userTexts(decoded)
	if len(texts) == 0 {
		return ""
	}
	return texts[0]
}

type childScript struct {
	mu      sync.Mutex
	replies map[string][]response
	seen    map[string]int
}

func (c *childScript) route(body []byte) (response, bool) {
	prompt := firstUserText(body)
	c.mu.Lock()
	defer c.mu.Unlock()
	queue, ok := c.replies[prompt]
	if !ok {
		return response{}, false
	}
	index := min(c.seen[prompt], len(queue)-1)
	c.seen[prompt]++
	return queue[index], true
}

func (c *childScript) requests(prompt string) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.seen[prompt]
}

func newChildScript(replies map[string][]response) *childScript {
	return &childScript{replies: replies, seen: make(map[string]int)}
}

func spawnTwo() response {
	return calls(
		call{id: "ta", name: "task", args: `{"prompt":"child a"}`},
		call{id: "tb", name: "task", args: `{"prompt":"child b"}`},
	)
}

func yieldData(id, data string) response {
	return calls(call{id: id, name: "yield", args: `{"data":` + data + `}`})
}

type rendezvous struct {
	mu      sync.Mutex
	pending int
	gate    chan struct{}
}

func (r *rendezvous) arrive() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.pending--
	if r.pending == 0 {
		close(r.gate)
	}
}

func TestTaskChildrenRunTogetherAndYield(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	meet := &rendezvous{pending: 2, gate: make(chan struct{})}
	script := newChildScript(map[string][]response{
		"child a": {{body: yieldData("ya", `"A"`).body, gate: meet.gate}},
		"child b": {{body: yieldData("yb", `{"n":2}`).body, gate: meet.gate}},
	})
	h.model.route = func(body []byte) (response, bool) {
		reply, ok := script.route(body)
		if ok {
			go meet.arrive()
		}
		return reply, ok
	}
	h.model.push(spawnTwo(), text("parent done"))
	parent := h.idleThread("spawn")
	outputs := callOutputs(h.model.request(h.model.count() - 1))
	if outputs["ta"] != `{"data":"A"}` || outputs["tb"] != `{"data":{"n":2}}` {
		t.Fatalf("task outputs = %v", outputs)
	}
	if script.requests("child a") != 1 || script.requests("child b") != 1 {
		t.Fatalf("a yield must end the child turn: %d, %d requests", script.requests("child a"), script.requests("child b"))
	}
	spawned, threads := countType(h.events(parent.ID()), event.TypeChildSpawned), h.host.Threads()
	if spawned != 2 || len(threads) != 3 {
		t.Fatalf("child.spawned = %d, threads = %d", spawned, len(threads))
	}
}

func countType(records []event.Seqd, kind string) int {
	count := 0
	for _, item := range records {
		if item.Event.Type() == kind {
			count++
		}
	}
	return count
}

func TestChildWithoutYieldGetsOneNudge(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	script := newChildScript(map[string][]response{"child a": {text("first answer"), text("second answer")}})
	h.model.route = script.route
	h.model.push(calls(call{id: "ta", name: "task", args: `{"prompt":"child a"}`}), text("parent done"))
	h.idleThread("spawn")
	want := "subagent ended without yield; last message: second answer"
	if got := callOutputs(h.model.request(h.model.count() - 1))["ta"]; got != want {
		t.Fatalf("task output = %q", got)
	}
	if script.requests("child a") != 2 {
		t.Fatalf("child requests = %d", script.requests("child a"))
	}
}

func TestYieldNeedsExactlyOneField(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	script := newChildScript(map[string][]response{"child a": {
		calls(call{id: "y1", name: "yield", args: `{"data":1,"error":"x"}`}),
		yieldData("y2", "1"),
	}})
	h.model.route = script.route
	h.model.push(calls(call{id: "ta", name: "task", args: `{"prompt":"child a"}`}), text("parent done"))
	h.idleThread("spawn")
	child := h.host.Threads()
	var childID string
	for _, info := range child {
		if info.Parent != "" {
			childID = info.ID
		}
	}
	outputs := completedOutputs(h.events(childID))
	if outputs["y1"].OK || outputs["y1"].Output != "yield needs exactly one of data or error" || !outputs["y2"].OK {
		t.Fatalf("yield outputs = %+v", outputs)
	}
}

func copyHome(t *testing.T, from, to string) {
	t.Helper()
	source, err := os.OpenRoot(from)
	if err != nil {
		t.Fatal(err)
	}
	defer source.Close()
	err = fs.WalkDir(source.FS(), ".", func(rel string, entry fs.DirEntry, err error) error {
		target := filepath.Join(to, rel)
		switch {
		case err != nil:
			return err
		case entry.IsDir():
			return os.MkdirAll(target, 0o700)
		case strings.HasPrefix(entry.Name(), "state.db") || entry.Name() == "lock" || strings.Contains(rel, "runs"):
			return nil
		}
		data, err := source.ReadFile(rel)
		if err != nil {
			return err
		}
		return os.WriteFile(target, data, 0o600)
	})
	if err != nil {
		t.Fatal(err)
	}
}

func TestDaemonCrashResumesParentAndChildren(t *testing.T) {
	t.Parallel()
	crashed := newHarness(t)
	hold := make(chan struct{})
	t.Cleanup(func() { close(hold) })
	crashed.model.route = newChildScript(map[string][]response{
		"child a": {{body: sseText("never"), gate: hold}}, "child b": {{body: sseText("never"), gate: hold}},
	}).route
	crashed.model.push(spawnTwo())
	parent := crashed.start()
	crashed.submit(parent, "spawn")
	crashed.waitRequests(3)
	home := t.TempDir()
	copyHome(t, crashed.home, home)

	script := newChildScript(map[string][]response{"child a": {yieldData("ya", `"A"`)}, "child b": {yieldData("yb", `"B"`)}})
	revived := newHarness(t, func(h *harness) { h.cwd, h.cfg.Config.Cwd = crashed.cwd, crashed.cwd })
	revived.model.route = script.route
	revived.model.push(text("parent resumed"))
	revivedHost := revived.reopen(home)
	if err := revivedHost.Recover(t.Context()); err != nil {
		t.Fatal(err)
	}
	resumed, err := revivedHost.Resume(t.Context(), parent.ID())
	if err != nil {
		t.Fatal(err)
	}
	revived.waitIdle(resumed)
	outputs := completedOutputs(revived.events(parent.ID()))
	if outputs["ta"].Output != `{"data":"A"}` || outputs["tb"].Output != `{"data":"B"}` {
		for _, info := range revivedHost.Threads() {
			t.Logf("thread %s parent=%s events=%v", info.ID, info.Parent, revived.types(info.ID))
		}
		t.Fatalf("recovered task outputs = %+v", outputs)
	}
	if threads := revivedHost.Threads(); len(threads) != 3 {
		t.Fatalf("recovery re-spawned children: %d threads", len(threads))
	}
}

func (h *harness) reopen(home string) *Host {
	h.t.Helper()
	store, err := journal.Open(home)
	if err != nil {
		h.t.Fatal(err)
	}
	h.t.Cleanup(func() { _ = store.Close() })
	h.home, h.cfg.Config.Home, h.journal, h.cfg.Journal = home, home, store, store
	h.host = h.newHost()
	return h.host
}
