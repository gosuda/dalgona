package tui

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type resumeSearchCaller struct {
	fakeCaller
	search func(context.Context, string, string) (string, error)
}

func (c *resumeSearchCaller) Call(ctx context.Context, method string, params, result any) error {
	if method != "dalgona/thread/search" {
		return c.fakeCaller.Call(ctx, method, params, result)
	}
	wire, err := json.Marshal(params)
	if err != nil {
		return err
	}
	var request struct {
		Query  string `json:"query"`
		Cursor string `json:"cursor"`
	}
	if err = json.Unmarshal(wire, &request); err != nil {
		return err
	}
	reply, err := c.search(ctx, request.Query, request.Cursor)
	if err != nil {
		return err
	}
	return json.Unmarshal([]byte(reply), result)
}

func recentResumeList(t *testing.T, client caller) model {
	t.Helper()
	m := newTestModel(client, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	var recent rpc.ThreadListResponse
	for i := range 50 {
		recent.Data = append(recent.Data, rpc.Thread{ID: fmt.Sprintf("recent-%d", i), Name: fmt.Sprintf("Recent %02d", i)})
	}
	m, _ = drive(t, m, resultMsg{op: "threads", value: &recent})
	return m
}

func runResumeInput(t *testing.T, m model, msg tea.Msg) model {
	t.Helper()
	next, command := drive(t, m, msg)
	next, _ = runCommands(t, next, command)
	return next
}

func TestResumeSearchOpensOlderThreadByMessage(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var queries []string
		client := &resumeSearchCaller{search: func(_ context.Context, query, _ string) (string, error) {
			queries = append(queries, query)
			return `{"data":[{"threadId":"old-51","name":"Deploy notes","snippet":"\uc11c\ubc84\uc751\ub2f5\n timed out\u001b]52;c;hidden-control\u0007"}]}`, nil
		}}
		m := recentResumeList(t, client)
		m = runResumeInput(t, m, press('\uc11c', 0, "\uc11c"))
		m = runResumeInput(t, m, press('\ubc84', 0, "\ubc84"))
		m = runResumeInput(t, m, tea.PasteMsg{Content: "\uc751\ub2f5"})
		if !slices.Equal(queries, []string{"\uc11c\ubc84\uc751\ub2f5"}) {
			t.Fatalf("searches = %v, want just the three-character query", queries)
		}
		assertContains(t, stripANSI(m.View().Content), "Deploy notes", "\uc11c\ubc84\uc751\ub2f5 timed out")
		if strings.Contains(m.View().Content, "hidden-control") {
			t.Fatal("the excerpt emitted a terminal control sequence")
		}
		m = runResumeInput(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
		for _, call := range client.calls {
			if call.method == "thread/resume" && call.params.(rpc.ThreadResumeParams).ThreadID == "old-51" && m.list == nil {
				return
			}
		}
		t.Fatalf("Enter did not resume the older match: %+v", client.calls)
	})
}

func TestResumeSearchPreservesTitleSelectionAcrossPagesAndRetry(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var cursors []string
		client := &resumeSearchCaller{search: func(_ context.Context, query, cursor string) (string, error) {
			if query != "cent" {
				t.Fatalf("unexpected search %q", query)
			}
			cursors = append(cursors, cursor)
			switch len(cursors) {
			case 1:
				return `{"data":[{"threadId":"recent-49","name":"Recent 49","snippet":"recognisable excerpt"}],"nextCursor":"1"}`, nil
			case 2:
				return "", errors.New("temporarily unavailable")
			default:
				return `{"data":[{"threadId":"recent-49","snippet":"recognisable excerpt"},{"threadId":"older","name":"Deployment","snippet":"last match"}]}`, nil
			}
		}}
		m := recentResumeList(t, client)
		m, search := drive(t, m, tea.PasteMsg{Content: "cent"})
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
		m, _ = runCommands(t, m, search)
		assertContains(t, stripANSI(m.View().Content), "recognisable excerpt", "Tab")
		m = runResumeInput(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
		assertContains(t, stripANSI(m.View().Content), "temporarily unavailable", "recognisable excerpt")
		m = runResumeInput(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
		m = runResumeInput(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
		if !slices.Equal(cursors, []string{"", "1", "1"}) {
			t.Fatalf("page cursors = %v", cursors)
		}
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
		assertContains(t, stripANSI(m.View().Content), "Deployment", "last match")
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
		if view := stripANSI(m.View().Content); strings.Contains(view, "last match") || !strings.Contains(view, "Recent 00") {
			t.Fatalf("clearing the filter did not restore the recent list:\n%s", view)
		}
	})
}

func TestResumeSearchKeepsRecentStateAndListDate(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		now := time.Date(2026, 12, 31, 23, 59, 59, 0, time.UTC)
		client := &resumeSearchCaller{search: func(context.Context, string, string) (string, error) {
			return fmt.Sprintf(`{"data":[{"threadId":"active","name":"Current work"},{"threadId":"old","name":"Old project","updatedAt":%d}]}`, now.Unix()-1), nil
		}}
		m := newTestModel(client, Options{})
		m.conn.clock = func() time.Time { return now }
		m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
		recent := rpc.Thread{ID: "active", Name: "Current work", Status: rpc.ThreadStatus{Type: "active"}, CreatedAt: now.Add(-24 * time.Hour).Unix()}
		m, _ = drive(t, m, resultMsg{op: "threads", value: &rpc.ThreadListResponse{Data: []rpc.Thread{recent}}})
		now = now.Add(time.Second)
		m = runResumeInput(t, m, tea.PasteMsg{Content: "needle"})
		assertContains(t, stripANSI(m.View().Content), "Current work", "running", "12-30", "Old project", "12-31")
	})
}

func TestResumeSearchDiscardsLateReplyForSameQuery(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		started := make(chan string, 3)
		release := make(chan struct{})
		defer close(release)
		first := true
		client := &resumeSearchCaller{search: func(_ context.Context, query, _ string) (string, error) {
			started <- query
			if first {
				first = false
				<-release
				return `{"data":[{"threadId":"stale","name":"Stale result","snippet":"obsolete excerpt"}]}`, nil
			}
			return `{"data":[]}`, nil
		}}
		m := recentResumeList(t, client)
		m, typing := drive(t, m, tea.PasteMsg{Content: "nee"})
		m, search := drive(t, m, press('d', 0, "d"))
		late := asyncResumeMsgs(search)
		awaitDebouncedSearch(t, started)
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
		m, middle := drive(t, m, tea.PasteMsg{Content: "discard"})
		obsolete := asyncResumeMsgs(middle)
		synctest.Wait()
		pause(300 * time.Millisecond)
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
		m, last := drive(t, m, tea.PasteMsg{Content: "need"})
		fresh := asyncResumeMsgs(last)
		synctest.Wait()
		pause(300 * time.Millisecond)
		if len(started) != 0 {
			t.Fatal("another search reached the daemon while the obsolete call was running")
		}
		release <- struct{}{}
		for _, msg := range slices.Concat(<-fresh, <-late, <-obsolete, commandMsgs(typing)) {
			m, _ = drive(t, m, msg)
		}
		if view := stripANSI(m.View().Content); strings.Contains(view, "Stale result") {
			t.Fatalf("obsolete reply replaced the new search:\n%s", view)
		}
		m = runResumeInput(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
		assertContains(t, stripANSI(m.View().Content), "No conversation mentions this text.")
		if len(started) != 1 || <-started != "need" || len(client.calls) != 0 {
			t.Fatalf("obsolete search or empty selection sent another request: %+v", client.calls)
		}
	})
}

func asyncResumeMsgs(cmd tea.Cmd) <-chan []tea.Msg {
	msgs := make(chan []tea.Msg, 1)
	go func() { msgs <- commandMsgs(cmd) }()
	return msgs
}

func awaitDebouncedSearch(t *testing.T, started <-chan string) {
	t.Helper()
	synctest.Wait()
	pause(299 * time.Millisecond)
	if len(started) != 0 {
		t.Fatal("conversation search started during the debounce")
	}
	pause(time.Millisecond)
	select {
	case query := <-started:
		if query != "need" {
			t.Fatalf("search = %q, want need", query)
		}
	default:
		t.Fatal("the stable filter did not start a search")
	}
}

func TestResumeSearchOnlyUsesTheVisibleFilter(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		client := &resumeSearchCaller{search: func(_ context.Context, query, _ string) (string, error) {
			t.Fatalf("unexpected conversation search for %q", query)
			return "", nil
		}}
		m := recentResumeList(t, client)
		m = runResumeInput(t, m, tea.PasteMsg{Content: "disk io"})
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
		m, pending := drive(t, m, tea.PasteMsg{Content: "deploy"})
		free := `{"threadId":"t1","turnId":"one","itemId":"ask","questions":[{"id":"q","header":"Label","question":"Which label?","options":null}]}`
		m, _ = drive(t, m, questionRequest(make(chan requestAnswer, 1), free))
		if m.textMode() != inputAnswer {
			t.Fatal("expected an answer field above the picker")
		}
		m = runResumeInput(t, m, tea.PasteMsg{Content: "unrelated answer"})
		m, _ = runCommands(t, m, pending)
		m = runResumeInput(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
		if m.textMode() != inputFilter || m.input.field.value() != "" {
			t.Fatal("dismissing the question did not restore an empty resume filter")
		}
	})
}

func TestResumeSearchRejectsOversizePasteBeforeReadingItsFirstLine(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var queries []string
		client := &resumeSearchCaller{search: func(_ context.Context, query, _ string) (string, error) {
			queries = append(queries, query)
			return `{"data":[{"threadId":"saved","name":"Saved conversation","snippet":"matched excerpt"}]}`, nil
		}}
		m := recentResumeList(t, client)
		m = runResumeInput(t, m, tea.PasteMsg{Content: "deploy"})
		m = runResumeInput(t, m, tea.PasteMsg{Content: "bad\n" + strings.Repeat("x", 1<<20)})
		if !slices.Equal(queries, []string{"deploy"}) || m.input.field.value() != "deploy" {
			t.Fatalf("oversize paste edited the filter or started a search: %q, %v", m.input.field.value(), queries)
		}
		assertContains(t, m.notice, "1 MiB")
		assertContains(t, stripANSI(m.View().Content), "matched excerpt")
	})
}

func TestResumeSearchSendsTheExactQuery(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var queries []string
		client := &resumeSearchCaller{search: func(_ context.Context, query, _ string) (string, error) {
			queries = append(queries, query)
			return `{"data":[]}`, nil
		}}
		m := recentResumeList(t, client)
		runResumeInput(t, m, tea.PasteMsg{Content: " disk\u3000logs "})
		if !slices.Equal(queries, []string{" disk\u3000logs "}) {
			t.Fatalf("searches = %q, want the filter with its spaces", queries)
		}
	})
}
