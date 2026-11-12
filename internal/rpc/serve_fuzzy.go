package rpc

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/tool/search"
)

const (
	fuzzyWalkLimit   = 100000
	fuzzyResultLimit = 50
	fuzzyQueryLimit  = 256
	fuzzyRootLimit   = 8
	fuzzyListTTL     = 5 * time.Second
)

var (
	fuzzyFiles = search.Files
	fuzzyNow   = time.Now
	fuzzyLists = &fuzzyListCache{entries: map[string]fuzzyListing{}, walking: map[string]chan struct{}{}}
)

type fuzzyListing struct {
	at    time.Time
	paths []string
}

type fuzzyListCache struct {
	mu      sync.Mutex
	entries map[string]fuzzyListing
	walking map[string]chan struct{}
}

func (c *fuzzyListCache) paths(root string) ([]string, error) {
	for {
		c.mu.Lock()
		if hit, ok := c.entries[root]; ok && fuzzyNow().Sub(hit.at) < fuzzyListTTL {
			c.mu.Unlock()
			return hit.paths, nil
		}
		if wait, busy := c.walking[root]; busy {
			c.mu.Unlock()
			<-wait
			continue
		}
		done := make(chan struct{})
		c.walking[root] = done
		c.mu.Unlock()
		return c.fill(root, done)
	}
}

func (c *fuzzyListCache) fill(root string, done chan struct{}) ([]string, error) {
	paths, err := fuzzyFiles(root, fuzzyWalkLimit)
	c.mu.Lock()
	defer c.mu.Unlock()
	delete(c.walking, root)
	close(done)
	if err != nil {
		return nil, err
	}
	now := fuzzyNow()
	maps.DeleteFunc(c.entries, func(_ string, hit fuzzyListing) bool {
		return now.Sub(hit.at) >= fuzzyListTTL
	})
	c.entries[root] = fuzzyListing{at: now, paths: paths}
	return paths, nil
}

type fuzzySearch struct {
	cancel context.CancelFunc
}

type fuzzyParams struct {
	query string
	roots []string
	token string
}

func decodeFuzzyParams(params jsontext.Value) (fuzzyParams, error) {
	var p fuzzyParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "query":
			p.query, err = readString(d)
		case "roots":
			err = readArray(d, func() error {
				root, readErr := readString(d)
				p.roots = append(p.roots, root)
				return readErr
			})
		case "cancellationToken":
			p.token, err = readString(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	return p, err
}

func (p fuzzyParams) validate() error {
	if p.query == "" {
		return invalidParams("query must not be empty")
	}
	if len(p.query) > fuzzyQueryLimit {
		return invalidParams("query is longer than 256 bytes")
	}
	if len(p.roots) == 0 || len(p.roots) > fuzzyRootLimit {
		return invalidParams("roots must hold 1 to 8 directories")
	}
	for _, root := range p.roots {
		if !filepath.IsAbs(root) {
			return invalidParams("root " + root + " must be absolute")
		}
		if info, err := os.Stat(root); err != nil || !info.IsDir() {
			return invalidParams("root " + root + " is not a directory")
		}
	}
	return nil
}

type fuzzyMatch struct {
	root, path string
	score      uint32
	indices    []uint32
}

type fuzzyResponse []fuzzyMatch

func (r fuzzyResponse) write(o *out) {
	o.open()
	o.key("files")
	o.openArray()
	for _, m := range r {
		o.open()
		o.str("root", m.root)
		o.str("path", m.path)
		o.str("file_name", filepath.Base(m.path))
		o.str("match_type", "file")
		o.int("score", int64(m.score))
		o.key("indices")
		o.openArray()
		for _, index := range m.indices {
			o.token(jsontext.Int(int64(index)))
		}
		o.closeArray()
		o.close()
	}
	o.closeArray()
	o.close()
}

func (s *server) fuzzyFileSearch(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFuzzyParams(params)
	if err != nil {
		return nil, err
	}
	if err = p.validate(); err != nil {
		return nil, err
	}
	ctx, done := s.beginFuzzy(ctx, p.token)
	defer done()
	matches, err := runFuzzy(ctx, p)
	if ctx.Err() != nil {
		return encode(fuzzyResponse(nil))
	}
	if err != nil {
		return nil, internalError(err)
	}
	return encode(fuzzyResponse(matches))
}

func (s *server) beginFuzzy(ctx context.Context, token string) (context.Context, func()) {
	if token == "" {
		return ctx, func() {}
	}
	ctx, cancel := context.WithCancel(ctx)
	current := &fuzzySearch{cancel: cancel}
	s.supersedeFuzzy(token, current)
	return ctx, func() {
		cancel()
		s.forgetFuzzy(token, current)
	}
}

func (s *server) supersedeFuzzy(token string, current *fuzzySearch) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.searches == nil {
		s.searches = map[string]*fuzzySearch{}
	}
	if previous := s.searches[token]; previous != nil {
		previous.cancel()
	}
	s.searches[token] = current
}

func (s *server) forgetFuzzy(token string, current *fuzzySearch) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.searches[token] == current {
		delete(s.searches, token)
	}
}

func runFuzzy(ctx context.Context, p fuzzyParams) ([]fuzzyMatch, error) {
	var matches []fuzzyMatch
	for _, root := range p.roots {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		paths, err := fuzzyLists.paths(root)
		if err != nil {
			return nil, err
		}
		for _, path := range paths {
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			if score, indices, ok := fuzzyScore(p.query, path); ok {
				matches = append(matches, fuzzyMatch{root: root, path: path, score: score, indices: indices})
			}
		}
	}
	slices.SortFunc(matches, compareFuzzy)
	return matches[:min(len(matches), fuzzyResultLimit)], nil
}

func compareFuzzy(a, b fuzzyMatch) int {
	if c := cmp.Compare(b.score, a.score); c != 0 {
		return c
	}
	if c := cmp.Compare(a.path, b.path); c != 0 {
		return c
	}
	return cmp.Compare(a.root, b.root)
}

func fuzzyScore(query, path string) (uint32, []uint32, bool) {
	needle := lowerRunes(query)
	folded := lowerRunes(path)
	window := fewestGapMatch(needle, folded)
	if window == nil {
		return 0, nil, false
	}
	first, last := window[0], window[len(window)-1]
	gaps := last - first + 1 - len(needle)
	base := baseStart(path)
	score := 100000 - 100*gaps - len(folded)
	if last >= base {
		score += 1000
	}
	if hasRunePrefix(folded[base:], needle) {
		score += 2000
	}
	indices := make([]uint32, len(window))
	for i, index := range window {
		//nolint:gosec // an index is a rune position in a path string, far below 2^32
		indices[i] = uint32(index)
	}
	//nolint:gosec // the score lies between 0 and about 103000, so the conversion fits
	return uint32(max(score, 0)), indices, true
}

func lowerRunes(s string) []rune {
	out := []rune(s)
	for i, r := range out {
		out[i] = unicode.ToLower(r)
	}
	return out
}

func baseStart(path string) int {
	dir, _, found := strings.CutLast(path, "/")
	if !found {
		return 0
	}
	return utf8.RuneCountInString(dir) + 1
}

func hasRunePrefix(s, prefix []rune) bool {
	return len(s) >= len(prefix) && slices.Equal(s[:len(prefix)], prefix)
}

func fewestGapMatch(needle, folded []rune) []int {
	var best []int
	if len(needle) == 0 {
		return nil
	}
	for start := range folded {
		if folded[start] != needle[0] {
			continue
		}
		window := greedyWindow(needle, folded, start)
		if window == nil {
			break
		}
		if best == nil || window[len(window)-1]-window[0] < best[len(best)-1]-best[0] {
			best = window
		}
	}
	return best
}

func greedyWindow(needle, folded []rune, start int) []int {
	window := []int{start}
	pos := start + 1
	for _, r := range needle[1:] {
		for pos < len(folded) && folded[pos] != r {
			pos++
		}
		if pos == len(folded) {
			return nil
		}
		window = append(window, pos)
		pos++
	}
	return window
}
