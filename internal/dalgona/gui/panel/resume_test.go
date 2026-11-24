//go:build gui

package panel

import (
	"slices"
	"testing"

	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func resumeHarness(t *testing.T) *harness {
	t.Helper()
	h := newHarness(t, endpointOf(local, "local", client.PhaseConnected), endpointOf(box, "box", client.PhaseConnected))
	h.snap.Lanes[local] = state.LaneView{Loaded: true, Rows: []rpc.Thread{{ID: "t1", Name: "Parser"}, {ID: "t2", Preview: "fix the lexer"}}}
	h.snap.Lanes[box] = state.LaneView{Loaded: true, More: true, Rows: []rpc.Thread{{ID: "t0", Name: "Infra"}, {ID: "t1", Name: "Deploy"}}}
	h.open(shell.Route{Panel: "resume"})
	return h
}

func (h *harness) items() []string {
	var out []string
	for _, n := range h.nodes {
		if n.Desc.Class == semantic.Button && n.Desc.Description != "" {
			out = append(out, n.Desc.Label+" @ "+n.Desc.Description)
		}
	}
	return out
}

func TestEqualThreadIDsOnTwoEndpointsStaySeparate(t *testing.T) {
	h := resumeHarness(t)
	if got := h.items(); !slices.Contains(got, "Parser @ local") || !slices.Contains(got, "Deploy @ box") {
		t.Fatalf("lane rows %q", got)
	}
	h.click("Deploy")
	if len(h.navigated) != 1 || h.navigated[0] != (state.ThreadKey{Endpoint: box, Thread: "t1"}) {
		t.Fatalf("choosing box t1 opened %v", h.navigated)
	}
}

func TestFilterMatchesTitlePreviewIdentifierAndEndpointAtOnce(t *testing.T) {
	h := resumeHarness(t)
	h.typeInto(&h.panels.resume.filter, "LEXER")
	if got := h.items(); !slices.Equal(got, []string{"fix the lexer @ local"}) {
		t.Fatalf("filter LEXER kept %q", got)
	}
	h.panels.resume.filter.clear()
	h.typeInto(&h.panels.resume.filter, "box")
	if got := h.items(); !slices.Equal(got, []string{"Infra @ box", "Deploy @ box"}) {
		t.Fatalf("filter by endpoint kept %q", got)
	}
}

func TestSearchStartsAfterTheDelayWithTheUnchangedQuery(t *testing.T) {
	h := resumeHarness(t)
	h.take()
	h.typeInto(&h.panels.resume.filter, " abc ")
	if slices.Contains(h.methods(), "dalgona/thread/search") {
		t.Fatal("search started before 300 milliseconds")
	}
	h.wait(searchDelay)
	var endpoints []client.EndpointID
	for _, c := range submitted[state.Call](h.intents) {
		if c.Method == "dalgona/thread/search" {
			endpoints = append(endpoints, c.Endpoint)
			if q := params[rpc.ThreadSearchParams](t, c).Query; q != " abc " {
				t.Fatalf("search sent %q, want the query unchanged", q)
			}
		}
	}
	slices.Sort(endpoints)
	if !slices.Equal(endpoints, []client.EndpointID{local, box}) && !slices.Equal(endpoints, []client.EndpointID{box, local}) {
		t.Fatalf("searched %v, want each connected endpoint once", endpoints)
	}
}

func TestShortTermsStayLocalFilters(t *testing.T) {
	for _, q := range []string{"ab ", "abc de"} {
		h := resumeHarness(t)
		h.typeInto(&h.panels.resume.filter, q)
		h.wait(searchDelay)
		if slices.Contains(h.methods(), "dalgona/thread/search") {
			t.Fatalf("%q started a conversation search", q)
		}
	}
}

func TestEachKeystrokeCancelsEarlierSearches(t *testing.T) {
	h := resumeHarness(t)
	h.typeInto(&h.panels.resume.filter, "parser")
	h.wait(searchDelay)
	first := submitted[state.Call](h.take())
	h.typeInto(&h.panels.resume.filter, "s")
	cancelled := submitted[state.CancelCall](h.intents)
	for _, c := range first {
		if c.Method == "dalgona/thread/search" && !slices.ContainsFunc(cancelled, func(x state.CancelCall) bool { return x.ID == c.ID }) {
			t.Fatalf("keystroke left search %s running", c.ID)
		}
	}
}

func searchCalls(h *harness) map[client.EndpointID]state.Call {
	out := map[client.EndpointID]state.Call{}
	for _, c := range submitted[state.Call](h.intents) {
		if c.Method == "dalgona/thread/search" {
			out[c.Endpoint] = c
		}
	}
	return out
}

func TestSearchOutcomesNameEndpointsAndKeepFailuresDistinct(t *testing.T) {
	h := resumeHarness(t)
	h.typeInto(&h.panels.resume.filter, "parser")
	h.wait(searchDelay)
	calls := searchCalls(h)
	h.answer(calls[local], rpc.ThreadSearchResponse{})
	h.fail(calls[box], "journal mirror is rebuilding")
	h.want("box · Search failed: journal mirror is rebuilding.")
	h.reject(noMentions)
	h.take()
	h.click("Try again")
	retry := searchCalls(h)
	if _, ok := retry[box]; !ok || len(retry) != 1 {
		t.Fatalf("Try again searched %v, want box only", retry)
	}
	h.answer(retry[box], rpc.ThreadSearchResponse{})
	h.want(noMentions)
}

func TestMatchesShowEndpointTitleAndSnippet(t *testing.T) {
	h := resumeHarness(t)
	h.typeInto(&h.panels.resume.filter, "parser")
	h.wait(searchDelay)
	calls := searchCalls(h)
	h.answer(calls[local], rpc.ThreadSearchResponse{Data: []rpc.ThreadSearchRow{{ThreadID: "t9", Name: "Old parser", Snippet: "the parser broke"}}})
	h.answer(calls[box], rpc.ThreadSearchResponse{Data: []rpc.ThreadSearchRow{{ThreadID: "t9", Name: "Box parser", Snippet: "parser on box"}}})
	got := h.items()
	if !slices.Contains(got, "Old parser @ local · the parser broke") || !slices.Contains(got, "Box parser @ box · parser on box") {
		t.Fatalf("matches %q", got)
	}
}

func TestOpeningKeepsThePanelUntilTheThreadOpens(t *testing.T) {
	h := resumeHarness(t)
	h.click("Parser")
	h.want("local · Opening thread")
	if n, ok := h.button("fix the lexer"); !ok || !n.Desc.Disabled {
		t.Fatal("choices stay enabled while opening")
	}
	if h.closed != 0 {
		t.Fatal("the panel closed before the thread opened")
	}
	h.snap.Threads = []*state.ThreadView{idleThread(state.ThreadKey{Endpoint: local, Thread: "t1"}, "Parser")}
	h.settle()
	if h.closed != 1 {
		t.Fatal("the panel stayed open after the thread opened")
	}
}

func TestOpeningFailureKeepsFilterAndResults(t *testing.T) {
	h := resumeHarness(t)
	h.typeInto(&h.panels.resume.filter, "par")
	h.click("Parser")
	failed := idleThread(state.ThreadKey{Endpoint: local, Thread: "t1"}, "Parser")
	failed.Missing = true
	h.snap.Threads = []*state.ThreadView{failed}
	h.settle()
	h.want("Unable to open the thread on local: the thread no longer exists")
	if h.panels.resume.filter.text() != "par" || !slices.Contains(h.items(), "Parser @ local") {
		t.Fatal("failure lost the filter or results")
	}
}

func TestLanesPageWithLoadMore(t *testing.T) {
	h := resumeHarness(t)
	h.take()
	h.click("Load more threads")
	loads := submitted[state.LoadThreads](h.intents)
	if len(loads) != 1 || loads[0].Endpoint != box {
		t.Fatalf("Load more loaded %v, want the box lane", loads)
	}
}
