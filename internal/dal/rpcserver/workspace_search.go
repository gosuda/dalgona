package rpcserver

import (
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"hash"
	"path"
	"slices"
	"time"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dal/tool/search"
)

const (
	workspaceSearchLimit  = 50
	workspaceSearchVisits = 10000
	workspaceSearchBudget = 5 * time.Second
	workspaceQueryBytes   = 1024
	workspaceSearchKeep   = 500
)

var workspaceQueryPolicy = textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: workspaceQueryBytes}}

type workspaceSpan struct {
	Start string `json:"start"`
	End   string `json:"end"`
}

type workspaceMatch struct {
	Path    string          `json:"path"`
	Score   uint32          `json:"score"`
	Matches []workspaceSpan `json:"matches"`
}

type workspaceSearchParams struct {
	WorkspaceID string `json:"workspaceId"`
	Query       string `json:"query"`
	Cursor      string `json:"cursor"`
	Limit       int    `json:"limit"`
}

type workspaceSearchPage struct {
	Entries    []workspaceMatch `json:"entries"`
	NextCursor string           `json:"nextCursor,omitzero"`
	Incomplete bool             `json:"incomplete"`
}

func textError(err error) error {
	coded := rpc.NativeKind(rpc.ErrorInvalidParams)
	coded.Message = err.Error()
	return coded
}

func (s *server) workspaceSearch(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceSearchParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	limit, err := pageLimit(p.Limit, workspaceSearchLimit, workspaceSearchLimit)
	if err != nil {
		return nil, err
	}
	if checkErr := textinput.CheckRequest([]textinput.Item{{Field: "query", Policy: workspaceQueryPolicy, Text: p.Query}}); checkErr != nil {
		return nil, textError(checkErr)
	}
	if p.Query == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceReader(ctx, p.WorkspaceID, "")
	if err != nil {
		return nil, err
	}
	defer use.release()
	found, err := s.searchResults(use, p)
	if err != nil {
		return nil, err
	}
	return s.searchPage(use.handle, found, limit)
}

func (s *server) searchResults(use workspaceUse, p workspaceSearchParams) (*workspaceCursor, error) {
	if p.Cursor != "" {
		return s.continueSearch(use, p)
	}
	walk := &workspaceWalk{s: s, query: p.Query, revision: sha256.New()}
	incomplete, err := walk.run(use, 0)
	if err != nil {
		return nil, err
	}
	slices.SortFunc(walk.matches, func(a, b workspaceMatch) int {
		return cmp.Or(cmp.Compare(b.Score, a.Score), cmp.Compare(a.Path, b.Path))
	})
	kept := walk.matches[:min(len(walk.matches), workspaceSearchKeep)]
	return &workspaceCursor{
		handle: use.handle, kind: searchCursor, path: p.Query, revision: walk.digest(),
		matches: kept, incomplete: incomplete, searchCut: walk.cut,
	}, nil
}

func (s *server) continueSearch(use workspaceUse, p workspaceSearchParams) (*workspaceCursor, error) {
	found, err := s.workspace.cursor(p.Cursor, use.handle, p.Query, searchCursor)
	if err != nil {
		return nil, err
	}
	walk := &workspaceWalk{s: s, revision: sha256.New()}
	incomplete, err := walk.run(use, found.searchCut.directories)
	if err != nil {
		return nil, err
	}
	if incomplete && walk.cut.directories < found.searchCut.directories && walk.visits < workspaceSearchVisits {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	s.workspace.forgetCursor(p.Cursor)
	revision := walk.digest()
	if walk.cut != found.searchCut || revision != found.revision {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: revision})
	}
	return found, nil
}

func (s *server) searchPage(h *workspaceHandle, found *workspaceCursor, limit int) (jsontext.Value, error) {
	end := min(found.offset+limit, len(found.matches))
	page := workspaceSearchPage{Entries: found.matches[found.offset:end], Incomplete: found.incomplete}
	if end < len(found.matches) {
		next := *found
		next.offset = end
		cursor, err := s.workspace.remember(&next)
		if err != nil {
			return nil, err
		}
		page.NextCursor = cursor
	}
	return json.Marshal(page)
}

type workspaceWalk struct {
	s        *server
	query    string
	rules    search.IgnoreRules
	visits   int
	matches  []workspaceMatch
	revision hash.Hash
	cut      workspaceSearchCut
}

type workspaceSearchCut struct {
	lastDir     string
	directories int
}

func (w *workspaceWalk) run(use workspaceUse, targetDirs int) (bool, error) {
	ctx, cancel := context.WithTimeout(use.ctx, workspaceSearchBudget)
	defer cancel()
	scoped := workspaceUse{ctx: ctx, handle: use.handle, release: use.release}
	exclude := w.readIgnore(scoped, ".git/info/exclude")
	w.applyIgnore("", ".git/info/exclude", exclude)
	queue := []string{""}
	for len(queue) > 0 {
		dir := queue[0]
		queue = queue[1:]
		next, err := w.walkDirectory(scoped, dir, queue)
		if err != nil {
			return workspaceWalkStopped(use.ctx, ctx, err)
		}
		w.cut = workspaceSearchCut{lastDir: dir, directories: w.cut.directories + 1}
		if w.cut.directories == targetDirs {
			break
		}
		queue = next
		if w.visits >= workspaceSearchVisits {
			return true, nil
		}
	}
	return false, nil
}

func workspaceWalkStopped(outer, budget context.Context, err error) (bool, error) {
	if outer.Err() != nil {
		return false, rpc.NativeKind(rpc.ErrorCancelled)
	}
	if errors.Is(budget.Err(), context.DeadlineExceeded) {
		return true, nil
	}
	return false, err
}

func (w *workspaceWalk) walkDirectory(use workspaceUse, dir string, queue []string) ([]string, error) {
	listing, err := w.s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceList, Path: dir})
	if err != nil && dir == "" {
		return nil, err
	}
	rel := path.Join(dir, ".gitignore")
	ignored := runner.WorkspaceResult{}
	if slices.ContainsFunc(listing.Entries, func(entry runner.WorkspaceEntry) bool { return entry.Name == ".gitignore" && entry.Kind == "file" }) {
		ignored = w.readIgnore(use, rel)
	}
	if ctxErr := use.ctx.Err(); ctxErr != nil {
		return nil, ctxErr
	}
	w.recordRevision(dir, listing.Revision)
	w.applyIgnore(dir, rel, ignored)
	for _, entry := range listing.Entries {
		queue = w.visitEntry(dir, entry, queue)
	}
	return queue, nil
}

func (w *workspaceWalk) visitEntry(dir string, entry runner.WorkspaceEntry, queue []string) []string {
	rel := path.Join(dir, entry.Name)
	directory := entry.Kind == "directory"
	switch {
	case entry.Kind == "symlink", directory && entry.Name == ".git", w.rules.Ignored(rel, directory):
	case directory:
		queue = append(queue, rel)
	case entry.Kind == "file" && w.visits < workspaceSearchVisits:
		w.visits++
		if w.query != "" {
			w.score(rel)
		}
	}
	return queue
}

func (w *workspaceWalk) readIgnore(use workspaceUse, rel string) runner.WorkspaceResult {
	result, err := w.s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceRead, Path: rel, Limit: runner.WorkspaceChunkLimit})
	if err != nil {
		return runner.WorkspaceResult{}
	}
	return result
}

func (w *workspaceWalk) applyIgnore(dir, rel string, result runner.WorkspaceResult) {
	if result.Revision != "" {
		w.recordRevision(rel, result.Revision)
		w.rules.Add(dir, string(result.Data))
	}
}

func (w *workspaceWalk) recordRevision(rel, revision string) {
	_, _ = w.revision.Write([]byte(rel))
	_, _ = w.revision.Write([]byte{0})
	_, _ = w.revision.Write([]byte(revision))
	_, _ = w.revision.Write([]byte{0})
}

func (w *workspaceWalk) digest() string { return hex.EncodeToString(w.revision.Sum(nil)) }

func (w *workspaceWalk) score(rel string) {
	score, indices, ok := fuzzyScore(w.query, rel)
	if ok {
		w.matches = append(w.matches, workspaceMatch{Path: rel, Score: score, Matches: byteSpans(rel, indices)})
	}
}

func byteSpans(text string, runeIndices []uint32) []workspaceSpan {
	spans := []workspaceSpan{}
	next := 0
	start, end := -1, -1
	runeIndex := uint32(0)
	for offset := range text {
		_, width := utf8.DecodeRuneInString(text[offset:])
		if next < len(runeIndices) && runeIndices[next] == runeIndex {
			next++
			if offset != end {
				spans = appendSpan(spans, start, end)
				start = offset
			}
			end = offset + width
		}
		runeIndex++
	}
	return appendSpan(spans, start, end)
}

func appendSpan(spans []workspaceSpan, start, end int) []workspaceSpan {
	if start < 0 || end < 0 {
		return spans
	}
	return append(spans, workspaceSpan{Start: rpc.FormatDecimal(uint64(start)), End: rpc.FormatDecimal(uint64(end))})
}
