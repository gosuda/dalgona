package rpc

import (
	"os"
	"path/filepath"
	"slices"
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

	if want := []string{"docs/serve.md", "internal/rpc/serve.go"}; !slices.Equal(fuzzyPaths(reply.Files), want) {
		t.Fatalf("paths=%v, want %v", fuzzyPaths(reply.Files), want)
	}
	first := reply.Files[0]
	if first.Root != root || first.FileName != "serve.md" || first.MatchType != "file" {
		t.Fatalf("first match=%+v", first)
	}
	if first.Score != 102987 {
		t.Fatalf("score=%d, want 102987", first.Score)
	}
	if want := []uint32{5, 6, 7, 8, 9}; !slices.Equal(first.Indices, want) {
		t.Fatalf("indices=%v, want %v", first.Indices, want)
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

func TestFuzzyScoreRule(t *testing.T) {
	cases := []struct {
		name, query, path string
		score             uint32
		indices           []uint32
		ok                bool
	}{
		{"base name prefix", "serve", "docs/serve.md", 102987, []uint32{5, 6, 7, 8, 9}, true},
		{"deeper path scores lower", "serve", "internal/rpc/serve.go", 102979, []uint32{13, 14, 15, 16, 17}, true},
		{"case insensitive", "SERVE", "docs/Serve.md", 102987, []uint32{5, 6, 7, 8, 9}, true},
		{"not a subsequence", "sv", "ignored/xyz", 0, nil, false},
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
