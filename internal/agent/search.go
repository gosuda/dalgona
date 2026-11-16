package agent

import (
	"context"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/journal"
)

type ThreadSearch struct {
	Text     string
	Cwd      string
	Archived bool
	Limit    int
	Offset   int
}

type ThreadHit struct {
	Info    ThreadInfo
	Seq     uint64
	Kind    string
	Snippet string
	Matches int
}

const (
	searchSessionCap   = 1000
	snippetRuneLimit   = 160
	snippetRuneLead    = 40
	snippetEllipsis    = "…"
	minimumSearchRunes = 3
)

func (h *Host) SearchThreads(ctx context.Context, q ThreadSearch) ([]ThreadHit, bool, error) {
	masked, err := h.mask(q.Text)
	if err != nil {
		return nil, false, err
	}
	hits, err := h.journal.Search(ctx, journal.SearchQuery{Text: masked, Limit: searchSessionCap})
	if err != nil {
		return nil, false, err
	}
	terms := searchTerms(masked)
	matched, err := h.matchedThreadHits(hits, h.threadIndex(), q, terms)
	if err != nil {
		return nil, false, err
	}
	start := min(max(q.Offset, 0), len(matched))
	end := min(start+max(q.Limit, 0), len(matched))
	return matched[start:end], end < len(matched), nil
}

func (h *Host) threadIndex() map[string]ThreadInfo {
	threads := h.AllThreads()
	byID := make(map[string]ThreadInfo, len(threads))
	for _, info := range threads {
		byID[info.ID] = info
	}
	return byID
}

func (h *Host) matchedThreadHits(hits []journal.SearchHit, byID map[string]ThreadInfo, q ThreadSearch, terms []string) ([]ThreadHit, error) {
	matched := make([]ThreadHit, 0, len(hits))
	for _, hit := range hits {
		info, ok := byID[hit.Session]
		if !ok {
			continue
		}
		if info.Archived != q.Archived {
			continue
		}
		if q.Cwd != "" && info.Cwd != q.Cwd {
			continue
		}
		snippet, err := h.mask(snippetOf(hit.Body, terms))
		if err != nil {
			return nil, err
		}
		matched = append(matched, ThreadHit{Info: info, Seq: hit.Seq, Kind: hit.Kind, Snippet: snippet, Matches: hit.Matches})
	}
	return matched, nil
}

func searchTerms(text string) []string {
	var terms []string
	for term := range strings.FieldsSeq(text) {
		if utf8.RuneCountInString(term) >= minimumSearchRunes {
			terms = append(terms, term)
		}
	}
	return terms
}

func snippetOf(body string, terms []string) string {
	flat := strings.ReplaceAll(body, "\n", " ")
	runes := []rune(flat)
	start := max(firstTermRune(flat, terms), 0)
	begin := max(start-snippetRuneLead, 0)
	end := min(begin+snippetRuneLimit, len(runes))
	prefix, suffix := "", ""
	if begin > 0 {
		prefix = snippetEllipsis
	}
	if end < len(runes) {
		suffix = snippetEllipsis
	}
	return prefix + string(runes[begin:end]) + suffix
}

func firstTermRune(flat string, terms []string) int {
	lower := strings.ToLower(flat)
	best := -1
	for _, term := range terms {
		at := strings.Index(lower, strings.ToLower(term))
		if at >= 0 && (best < 0 || at < best) {
			best = at
		}
	}
	if best < 0 {
		return -1
	}
	return utf8.RuneCountInString(flat[:best])
}
