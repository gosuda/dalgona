package agent

import (
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
)

func searchThreadWithText(t *testing.T, h *harness, host *Host, prompt string) *Thread {
	t.Helper()
	thread, err := host.Start(h.t.Context(), ThreadSpec{})
	if err != nil {
		t.Fatal(err)
	}
	h.submit(thread, prompt)
	h.waitIdle(thread)
	return thread
}

func TestSearchMasksQueryAndSnippet(t *testing.T) {
	const secret = "hunter2-hunter2"
	h := newHarness(t)
	rawThread := searchThreadWithText(t, h, h.host, "second needle with password "+secret)
	t.Setenv("TEST_PASSWORD", secret)
	store, err := auth.Load(h.home)
	if err != nil {
		t.Fatal(err)
	}
	h.cfg.Auth = store
	maskedHost := h.newHost()
	maskedThread := searchThreadWithText(t, h, maskedHost, "first needle with password "+secret)
	requireSecretQueryHitsOnlyMaskedThread(t, maskedHost, maskedThread, secret)
	requireNeedleHitsCoverThreads(t, maskedHost, rawThread, maskedThread, secret)
}

func requireSecretQueryHitsOnlyMaskedThread(t *testing.T, host *Host, maskedThread *Thread, secret string) {
	t.Helper()
	hits, more, err := host.SearchThreads(t.Context(), ThreadSearch{Text: "needle " + secret, Limit: 10})
	if err != nil {
		t.Fatalf("SearchThreads: %v", err)
	}
	if more {
		t.Fatal("more = true, want false")
	}
	if len(hits) != 1 || hits[0].Info.ID != maskedThread.ID() {
		t.Fatalf("hits = %+v, want only %s", hits, maskedThread.ID())
	}
	requireMaskedSnippet(t, hits[0].Info.ID, hits[0].Snippet, secret)
}

func requireNeedleHitsCoverThreads(t *testing.T, host *Host, rawThread, maskedThread *Thread, secret string) {
	t.Helper()
	all, _, err := host.SearchThreads(t.Context(), ThreadSearch{Text: "needle", Limit: 10})
	if err != nil {
		t.Fatalf("SearchThreads needle: %v", err)
	}
	if len(all) != 2 {
		t.Fatalf("needle hits = %d, want 2", len(all))
	}
	found := map[string]bool{}
	for _, hit := range all {
		found[hit.Info.ID] = true
		requireMaskedSnippet(t, hit.Info.ID, hit.Snippet, secret)
	}
	if !found[rawThread.ID()] || !found[maskedThread.ID()] {
		t.Fatalf("needle hits cover %v, want both %s and %s", found, rawThread.ID(), maskedThread.ID())
	}
}

func requireMaskedSnippet(t *testing.T, threadID, snippet, secret string) {
	t.Helper()
	if strings.Contains(snippet, secret) {
		t.Fatalf("snippet for %s leaks the secret: %q", threadID, snippet)
	}
	if !strings.Contains(snippet, "{{secret:TEST_PASSWORD}}") {
		t.Fatalf("snippet for %s lacks the secret placeholder: %q", threadID, snippet)
	}
}

func TestSearchThreadsFiltersAndPages(t *testing.T) {
	h := newHarness(t)
	first := searchThreadWithText(t, h, h.host, "needle one")
	second := searchThreadWithText(t, h, h.host, "needle two")
	third := searchThreadWithText(t, h, h.host, "needle three")
	if err := h.host.Archive(h.t.Context(), third.ID()); err != nil {
		t.Fatalf("Archive: %v", err)
	}
	requireArchivedThreadSplit(t, h, third)
	other, err := h.host.Start(h.t.Context(), ThreadSpec{Cwd: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	h.submit(other, "needle elsewhere")
	h.waitIdle(other)
	requireCwdScopedHits(t, h, other)
	page, rest := requirePagedHits(t, h)
	requirePagesCoverThreads(t, page, rest, first.ID(), second.ID(), other.ID())
}

func requireArchivedThreadSplit(t *testing.T, h *harness, third *Thread) {
	t.Helper()
	active, more, err := h.host.SearchThreads(h.t.Context(), ThreadSearch{Text: "needle", Limit: 10})
	if err != nil {
		t.Fatalf("SearchThreads: %v", err)
	}
	if more || len(active) != 2 {
		t.Fatalf("active hits = %+v more = %v, want 2 and false", active, more)
	}
	archived, _, err := h.host.SearchThreads(h.t.Context(), ThreadSearch{Text: "needle", Archived: true, Limit: 10})
	if err != nil {
		t.Fatalf("SearchThreads archived: %v", err)
	}
	if len(archived) != 1 || archived[0].Info.ID != third.ID() {
		t.Fatalf("archived hits = %+v, want %s", archived, third.ID())
	}
}

func requireCwdScopedHits(t *testing.T, h *harness, other *Thread) {
	t.Helper()
	var otherCwd string
	for _, info := range h.host.AllThreads() {
		if info.ID == other.ID() {
			otherCwd = info.Cwd
		}
	}
	if otherCwd == h.cwd {
		t.Fatal("thread cwd equals the home cwd")
	}
	scoped, _, err := h.host.SearchThreads(h.t.Context(), ThreadSearch{Text: "needle", Cwd: otherCwd, Limit: 10})
	if err != nil {
		t.Fatalf("SearchThreads scoped: %v", err)
	}
	if len(scoped) != 1 || scoped[0].Info.ID != other.ID() {
		t.Fatalf("scoped hits = %+v, want %s", scoped, other.ID())
	}
}

func requirePagedHits(t *testing.T, h *harness) ([]ThreadHit, []ThreadHit) {
	t.Helper()
	page, more, err := h.host.SearchThreads(h.t.Context(), ThreadSearch{Text: "needle", Limit: 2})
	if err != nil {
		t.Fatalf("SearchThreads page: %v", err)
	}
	if len(page) != 2 || !more {
		t.Fatalf("first page = %+v more = %v, want 2 and true", page, more)
	}
	rest, more, err := h.host.SearchThreads(h.t.Context(), ThreadSearch{Text: "needle", Limit: 2, Offset: 2})
	if err != nil {
		t.Fatalf("SearchThreads rest: %v", err)
	}
	if len(rest) != 1 || more {
		t.Fatalf("second page = %+v more = %v, want 1 and false", rest, more)
	}
	return page, rest
}

func requirePagesCoverThreads(t *testing.T, page, rest []ThreadHit, ids ...string) {
	t.Helper()
	seen := map[string]bool{}
	for _, hits := range [][]ThreadHit{page, rest} {
		for _, hit := range hits {
			seen[hit.Info.ID] = true
			if !strings.Contains(hit.Snippet, "needle") {
				t.Fatalf("snippet for %s lacks the term: %q", hit.Info.ID, hit.Snippet)
			}
		}
	}
	for _, id := range ids {
		if !seen[id] {
			t.Fatalf("thread %s missing from the pages", id)
		}
	}
	if len(seen) != len(ids) {
		t.Fatalf("pages cover %d threads, want %d", len(seen), len(ids))
	}
}

func appendSearchSession(t *testing.T, h *harness, id, text string) {
	t.Helper()
	appendSearchEvents(t, h, id, text, "")
}

func appendEphemeralSearchSession(t *testing.T, h *harness, id, text string) {
	t.Helper()
	appendSearchEvents(t, h, id, text, `,"ephemeral":true`)
}

func appendSearchEvents(t *testing.T, h *harness, id, text, createdExtra string) {
	t.Helper()
	now := time.Now().UTC()
	created, err := event.Encode(event.SessionCreated{ID: id, Cwd: h.cwd, Model: "openai/gpt-test", Approval: "auto", Sandbox: "off", Runner: "local", CreatedAt: now})
	if err != nil {
		t.Fatalf("encode session.created: %v", err)
	}
	created = append(created[:len(created)-1], createdExtra+"}"...)
	raws := [][]byte{created}
	for _, ev := range []event.Event{
		event.TurnStarted{TurnID: "t1", StartedAt: now},
		event.UserMessage{ItemID: "i1", TurnID: "t1", Text: text, Kind: "prompt"},
		event.TurnCompleted{TurnID: "t1", Status: "completed", CompletedAt: now},
	} {
		raw, err := event.Encode(ev)
		if err != nil {
			t.Fatalf("encode %s: %v", ev.Type(), err)
		}
		raws = append(raws, raw)
	}
	if _, err := h.journal.Append(t.Context(), id, raws...); err != nil {
		t.Fatalf("append %s: %v", id, err)
	}
}

func TestSearchThreadsExcludesEphemeralThreads(t *testing.T) {
	h := newHarness(t)
	appendEphemeralSearchSession(t, h, "ephemeral-thread", "needle spoken inside the ephemeral session")
	appendSearchSession(t, h, "plain-thread", "needle spoken inside the plain session")
	host := h.newHost()
	listed := map[string]bool{}
	for _, info := range host.AllThreads() {
		listed[info.ID] = true
	}
	if !listed["ephemeral-thread"] || !listed["plain-thread"] {
		t.Fatalf("listed threads = %v, want both journal sessions", listed)
	}
	hits, more, err := host.SearchThreads(t.Context(), ThreadSearch{Text: "needle", Limit: 10})
	if err != nil {
		t.Fatalf("SearchThreads: %v", err)
	}
	if more || len(hits) != 1 || hits[0].Info.ID != "plain-thread" {
		t.Fatalf("hits = %+v more = %v, want only plain-thread", hits, more)
	}
}
