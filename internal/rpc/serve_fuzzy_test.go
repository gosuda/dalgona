package rpc

import (
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool/search"
)

type fuzzyFile struct {
	Root      string   `json:"root"`
	Path      string   `json:"path"`
	FileName  string   `json:"file_name"`
	MatchType string   `json:"match_type"`
	Score     uint32   `json:"score"`
	Indices   []uint32 `json:"indices"`
}

type fuzzyReply struct {
	Files []fuzzyFile `json:"files"`
}

func writeTree(t *testing.T, root string, files map[string]string) {
	t.Helper()
	for name, body := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o750); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
}

func fuzzyPaths(files []fuzzyFile) []string {
	paths := make([]string, 0, len(files))
	for _, f := range files {
		paths = append(paths, f.Path)
	}
	return paths
}

func TestFuzzyFileSearchRanksAndHonoursGitignore(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	root := t.TempDir()
	writeTree(t, root, map[string]string{
		"docs/serve.md":         "x",
		"internal/rpc/serve.go": "x",
		"ignored/serve.go":      "x",
		".gitignore":            "ignored/\n",
	})

	var reply fuzzyReply
	c.ok("fuzzyFileSearch", map[string]any{"query": "serve", "roots": []string{root}}, &reply)

	if want := []string{"internal/rpc/serve.go", "docs/serve.md"}; !slices.Equal(fuzzyPaths(reply.Files), want) {
		t.Fatalf("paths=%v, want %v", fuzzyPaths(reply.Files), want)
	}
	first, second := reply.Files[0], reply.Files[1]
	if first.Root != root || first.FileName != "serve.go" || first.MatchType != "file" {
		t.Fatalf("first match=%+v", first)
	}
	if first.Score != 102979 {
		t.Fatalf("score=%d, want 102979", first.Score)
	}
	if want := []uint32{13, 14, 15, 16, 17}; !slices.Equal(first.Indices, want) {
		t.Fatalf("indices=%v, want %v", first.Indices, want)
	}
	if second.Score != 102787 {
		t.Fatalf("second score=%d, want 102787", second.Score)
	}
	if want := []uint32{3, 6, 7, 8, 9}; !slices.Equal(second.Indices, want) {
		t.Fatalf("second indices=%v, want %v", second.Indices, want)
	}
}

func TestFuzzyFileSearchOrdersByTheGreedyMatch(t *testing.T) {
	cases := []struct {
		name   string
		files  []string
		want   []string
		scores []uint32
	}{
		{
			name:   "greedy and fewest-gap disagree: a-x-b a0 b4 gaps 3 scores 100695, a/x/xab a0 b6 gaps 5 scores 100493, x/ab.go scores 102993",
			files:  []string{"a/x/xab", "a-x-b", "x/ab.go"},
			want:   []string{"x/ab.go", "a-x-b", "a/x/xab"},
			scores: []uint32{102993, 100695, 100493},
		},
		{
			name:   "greedy and fewest-gap agree: x/ab.go has 7 runes and scores 102993, xyz/ab.go has 9 runes and scores 102991",
			files:  []string{"xyz/ab.go", "x/ab.go"},
			want:   []string{"x/ab.go", "xyz/ab.go"},
			scores: []uint32{102993, 102991},
		},
	}
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			root := t.TempDir()
			tree := make(map[string]string, len(tc.files))
			for _, name := range tc.files {
				tree[name] = "x"
			}
			writeTree(t, root, tree)
			var reply fuzzyReply
			c.ok("fuzzyFileSearch", map[string]any{"query": "ab", "roots": []string{root}}, &reply)
			scores := make([]uint32, 0, len(reply.Files))
			for _, f := range reply.Files {
				scores = append(scores, f.Score)
			}
			if !slices.Equal(fuzzyPaths(reply.Files), tc.want) || !slices.Equal(scores, tc.scores) {
				t.Fatalf("paths=%v scores=%v, want %v %v", fuzzyPaths(reply.Files), scores, tc.want, tc.scores)
			}
		})
	}
}

func TestFuzzyResultsSortByScoreThenPath(t *testing.T) {
	matches := []fuzzyMatch{
		{root: "/r", path: "ab/d", score: 99996},
		{root: "/r", path: "z", score: 100001},
		{root: "/r", path: "ab/c", score: 99996},
	}
	want := []string{"z", "ab/c", "ab/d"}
	for _, order := range [][]fuzzyMatch{matches, {matches[2], matches[1], matches[0]}} {
		sorted := slices.Clone(order)
		slices.SortFunc(sorted, compareFuzzy)
		got := make([]string, 0, len(sorted))
		for _, m := range sorted {
			got = append(got, m.path)
		}
		if !slices.Equal(got, want) {
			t.Fatalf("sorted=%v, want %v", got, want)
		}
	}
}

func TestFuzzyFileSearchReusesWalk(t *testing.T) {
	walks := 0
	files := fuzzyFiles
	now := fuzzyNow
	fuzzyFiles = func(root string, limit int) ([]string, error) {
		walks++
		return search.Files(root, limit)
	}
	clock := time.Date(2026, 11, 12, 9, 0, 0, 0, time.UTC)
	fuzzyNow = func() time.Time { return clock }
	t.Cleanup(func() {
		fuzzyFiles = files
		fuzzyNow = now
	})

	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	root := t.TempDir()
	writeTree(t, root, map[string]string{"a/serve.go": "x"})

	query := func() {
		var reply fuzzyReply
		c.ok("fuzzyFileSearch", map[string]any{"query": "serve", "roots": []string{root}}, &reply)
	}
	query()
	query()
	if walks != 1 {
		t.Fatalf("walks=%d after two requests in one window, want 1", walks)
	}
	clock = clock.Add(6 * time.Second)
	query()
	if walks != 2 {
		t.Fatalf("walks=%d after the window ended, want 2", walks)
	}
}

func TestFuzzyFileSearchSupersededTokenAnswersEmpty(t *testing.T) {
	previous := fuzzyFiles
	started := make(chan struct{})
	release := make(chan struct{})
	notify := sync.OnceFunc(func() { close(started) })
	big := t.TempDir()
	small := t.TempDir()
	writeTree(t, big, map[string]string{"a/serve.go": "x", "b/serve.go": "x"})
	writeTree(t, small, map[string]string{"serve.go": "x"})
	fuzzyFiles = func(root string, limit int) ([]string, error) {
		if root == big {
			notify()
			<-release
		}
		return search.Files(root, limit)
	}
	t.Cleanup(func() { fuzzyFiles = previous })

	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	finished := make(chan message, 1)
	go func() {
		finished <- c.call("fuzzyFileSearch", map[string]any{"query": "serve", "roots": []string{big}, "cancellationToken": "tok"})
	}()
	<-started
	var fresh fuzzyReply
	c.ok("fuzzyFileSearch", map[string]any{"query": "serve", "roots": []string{small}, "cancellationToken": "tok"}, &fresh)
	if len(fresh.Files) != 1 {
		t.Fatalf("superseding search returned %+v, want the small-root match", fresh.Files)
	}
	close(release)
	select {
	case m := <-finished:
		if m.Error != nil {
			t.Fatalf("superseded search failed: %d %s", m.Error.Code, m.Error.Message)
		}
		var superseded fuzzyReply
		if err := json.Unmarshal(m.Result, &superseded); err != nil {
			t.Fatal(err)
		}
		if len(superseded.Files) != 0 {
			t.Fatalf("superseded search returned %+v, want no files", superseded.Files)
		}
	case <-time.After(waitLimit):
		t.Fatal("superseded search did not answer")
	}

	var none fuzzyReply
	c.ok("fuzzyFileSearch", map[string]any{"query": "zzzz", "roots": []string{small}}, &none)
	if len(none.Files) != 0 {
		t.Fatalf("non-matching query returned %+v, want no files", none.Files)
	}
}

func TestFuzzyScoreRule(t *testing.T) {
	cases := []struct {
		name, query, path string
		score             uint32
		indices           []uint32
		ok                bool
	}{
		{"serve in docs/serve.md takes the s of docs at 3, gaps 2, runes 13, last in base, base prefix", "serve", "docs/serve.md", 102787, []uint32{3, 6, 7, 8, 9}, true},
		{"serve in internal/rpc/serve.go has no earlier s, gaps 0, runes 21, last in base, base prefix", "serve", "internal/rpc/serve.go", 102979, []uint32{13, 14, 15, 16, 17}, true},
		{"SERVE in docs/Serve.md folds case like docs/serve.md", "SERVE", "docs/Serve.md", 102787, []uint32{3, 6, 7, 8, 9}, true},
		{"sv in ignored/xyz has no s", "sv", "ignored/xyz", 0, nil, false},
		{"ba in ab has no a after the b", "ba", "ab", 0, nil, false},
		{"ab in a/x/xab takes a0 then b6, gaps 5, runes 7, last in base, no base prefix", "ab", "a/x/xab", 100493, []uint32{0, 6}, true},
		{"ab in a-x-b takes a0 then b4, gaps 3, runes 5, last in base, no base prefix", "ab", "a-x-b", 100695, []uint32{0, 4}, true},
		{"ab in x/ab.go takes a2 then b3, gaps 0, runes 7, last in base, base prefix", "ab", "x/ab.go", 102993, []uint32{2, 3}, true},
		{"ab in ab/ab takes the first window a0 b1, gaps 0, runes 5, last not in base, base prefix", "ab", "ab/ab", 101995, []uint32{0, 1}, true},
		{"ab in ab/zz takes a0 b1, gaps 0, runes 5, last not in base, no base prefix", "ab", "ab/zz", 99995, []uint32{0, 1}, true},
		{"go in \u00e9/go counts runes: g2 o3, gaps 0, runes 4, last in base, base prefix", "go", "\u00e9/go", 102996, []uint32{2, 3}, true},
		{"ab around 100000 filler runes clamps the score to 0", "ab", "a" + strings.Repeat("-", 100000) + "b", 0, []uint32{0, 100001}, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			score, indices, ok := fuzzyScore(tc.query, tc.path)
			if ok != tc.ok || score != tc.score || !slices.Equal(indices, tc.indices) {
				t.Fatalf("fuzzyScore(%q, %q)=%d %v %v, want %d %v %v", tc.query, tc.path, score, indices, ok, tc.score, tc.indices, tc.ok)
			}
		})
	}
}
