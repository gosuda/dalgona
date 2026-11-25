package state

import (
	"context"
	"encoding/base64"
	"slices"
	"testing"
)

func workspaceHandlers(p *fakePort) {
	p.handle(methodWorkspaceOpen, func(_ context.Context, call portCall) (any, error) {
		params := call.params.(workspaceOpenParams)
		return map[string]any{"workspaceId": "ws-" + params.Rights[0], "root": "/work/repo"}, nil
	})
	p.handle(methodWorkspaceSearch, func(_ context.Context, call portCall) (any, error) {
		params := call.params.(searchParams)
		return map[string]any{"entries": []map[string]any{{"path": params.Query + ".go", "score": 1, "matches": []any{}}}, "incomplete": false}, nil
	})
	p.handle(methodWorkspaceExec, func(context.Context, portCall) (any, error) {
		return map[string]any{"exitCode": 0, "durationMs": 3, "outputBase64": base64.StdEncoding.EncodeToString([]byte("ok\n")), "outputTruncated": false, "termination": "exited"}, nil
	})
}

func TestComposerSearchAndShellReuseOneHandlePerRight(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		workspaceHandlers(p)
		p.owner.Submit(SearchFiles{Key: localDraft, Query: "main", Ticket: 1})
		p.owner.Submit(SearchFiles{Key: localDraft, Query: "mai", Ticket: 2})
		p.owner.Submit(RunShell{Key: localDraft, Command: "ls", Ticket: 1})
		p.owner.Submit(RunShell{Key: localDraft, Command: "pwd", Ticket: 2})
		v := settled(p)
		search := v.FileSearch(localDraft)
		if got := [4]any{search.Ticket, search.Query, search.Root, search.Phase}; got != [4]any{uint64(2), "mai", "/work/repo", SearchDone} || !slices.Equal(search.Paths, []string{"mai.go"}) {
			t.Fatalf("search %+v, want ticket 2 for mai under /work/repo with mai.go", search)
		}
		runs := v.ShellRuns(localDraft)
		last := runs[len(runs)-1]
		if got := [5]any{len(runs), last.N, last.Phase, last.Output, last.OperationID != ""}; got != [5]any{2, 2, ShellExited, "ok\n", true} || last.ExitCode == nil {
			t.Fatalf("shell runs %+v", runs)
		}
		if opens := p.calls(methodWorkspaceOpen); len(opens) != 2 {
			t.Fatalf("opened %d workspace handles, want one read and one terminal", len(opens))
		}
	})
}
