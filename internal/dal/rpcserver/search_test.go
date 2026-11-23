package rpcserver

import (
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

type searchRow struct {
	ThreadID  string `json:"threadId"`
	Name      string `json:"name"`
	Preview   string `json:"preview"`
	Cwd       string `json:"cwd"`
	UpdatedAt int64  `json:"updatedAt"`
	Seq       int64  `json:"seq"`
	Kind      string `json:"kind"`
	Snippet   string `json:"snippet"`
	Matches   int    `json:"matches"`
}

type searchResult struct {
	Data       []searchRow `json:"data"`
	NextCursor string      `json:"nextCursor,omitzero"`
}

func searchTurn(t *testing.T, r *rig, c *wireClient, extra map[string]any, text string) string {
	t.Helper()
	id := startThread(t, r, c, extra)
	startTurn(t, c, id, text)
	c.collect()
	return id
}

func TestThreadSearchFindsText(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	id := searchTurn(t, r, c, nil, "needle prose here")
	var result searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "needle"}, &result)
	if len(result.Data) != 1 || result.NextCursor != "" {
		t.Fatalf("result = %+v, want one row and no cursor", result)
	}
	requireFoundSearchRow(t, r, result.Data[0], id)
	var absent searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "absentterm"}, &absent)
	if len(absent.Data) != 0 || absent.NextCursor != "" {
		t.Fatalf("absent result = %+v, want empty data", absent)
	}
}

func requireFoundSearchRow(t *testing.T, r *rig, row searchRow, id string) {
	t.Helper()
	if row.ThreadID != id || row.Kind != "user" || row.Matches != 1 || row.Seq < 1 {
		t.Fatalf("row = %+v", row)
	}
	if row.Cwd != r.cwd || row.UpdatedAt == 0 {
		t.Fatalf("row = %+v", row)
	}
	if row.Snippet != "needle prose here" {
		t.Fatalf("snippet = %q, want the whole short body", row.Snippet)
	}
}

func TestThreadSearchValidation(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	searchTurn(t, r, c, nil, "needle prose here")
	c.fails("dalgona/thread/search", map[string]any{}, rpc.CodeInvalidParams, "query is required")
	c.fails("dalgona/thread/search", map[string]any{"query": ""}, rpc.CodeInvalidParams, "query is required")
	c.fails("dalgona/thread/search", map[string]any{"query": "ab"}, rpc.CodeInvalidParams, "query must hold at least 3 characters")
	c.fails("dalgona/thread/search", map[string]any{"query": "needle ab"}, rpc.CodeInvalidParams, "query must hold at least 3 characters")
	c.fails("dalgona/thread/search", map[string]any{"query": "needle", "limit": -1}, rpc.CodeInvalidParams, "invalid limit -1")
	c.fails("dalgona/thread/search", map[string]any{"query": "needle", "cursor": "x"}, rpc.CodeInvalidParams, "invalid cursor x")
	c.fails("dalgona/thread/search", map[string]any{"query": "needle", "cursor": "-1"}, rpc.CodeInvalidParams, "invalid cursor -1")
	c.fails("dalgona/thread/search", map[string]any{"query": "needle", "cursor": "99999999999999999999"}, rpc.CodeInvalidParams, "invalid cursor 99999999999999999999")
	typed := c.call("dalgona/thread/search", map[string]any{"query": 5})
	if typed.Error == nil || typed.Error.Code != rpc.CodeInvalidParams {
		t.Fatalf("number query error = %+v, want %d", typed.Error, rpc.CodeInvalidParams)
	}
	var result searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "needle", "limit": 0}, &result)
	if len(result.Data) != 1 {
		t.Fatalf("zero-limit result = %+v, want the default limit", result)
	}
}

func TestThreadSearchPages(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	searchTurn(t, r, c, nil, "needle one")
	searchTurn(t, r, c, nil, "needle two")
	searchTurn(t, r, c, nil, "needle three")
	var first searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "needle", "limit": 2}, &first)
	if len(first.Data) != 2 || first.NextCursor != "2" {
		t.Fatalf("first page = %+v", first)
	}
	var second searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "needle", "limit": 2, "cursor": first.NextCursor}, &second)
	if len(second.Data) != 1 || second.NextCursor != "" {
		t.Fatalf("second page = %+v", second)
	}
	paged := map[string]bool{}
	for _, row := range slices.Concat(first.Data, second.Data) {
		paged[row.ThreadID] = true
	}
	if len(paged) != 3 {
		t.Fatalf("pages cover %d threads, want 3", len(paged))
	}
}

func TestThreadSearchFiltersArchivedAndCwd(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	kept := searchTurn(t, r, c, nil, "needle kept")
	archived := searchTurn(t, r, c, nil, "needle archived")
	c.ok("thread/archive", map[string]any{"threadId": archived}, nil)
	var active searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "needle"}, &active)
	if len(active.Data) != 1 || active.Data[0].ThreadID != kept {
		t.Fatalf("active result = %+v, want only %s", active, kept)
	}
	var gone searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "needle", "archived": true}, &gone)
	if len(gone.Data) != 1 || gone.Data[0].ThreadID != archived {
		t.Fatalf("archived result = %+v, want only %s", gone, archived)
	}
	other := t.TempDir()
	elsewhere := searchTurn(t, r, c, map[string]any{"cwd": other}, "needle elsewhere")
	var scoped searchResult
	c.ok("dalgona/thread/search", map[string]any{"query": "needle", "cwd": other}, &scoped)
	if len(scoped.Data) != 1 || scoped.Data[0].ThreadID != elsewhere {
		t.Fatalf("scoped result = %+v, want only %s", scoped, elsewhere)
	}
}
