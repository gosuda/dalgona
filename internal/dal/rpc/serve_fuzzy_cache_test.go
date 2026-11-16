package rpc

import (
	"slices"
	"sync"
	"testing"
	"time"
)

func TestFuzzyCacheHitNotBlockedByWalk(t *testing.T) {
	now := time.Date(2026, 11, 12, 9, 0, 0, 0, time.UTC)
	clock := fuzzyNow
	files := fuzzyFiles
	fuzzyNow = func() time.Time { return now }
	started := make(chan struct{})
	release := make(chan struct{})
	slowDone := make(chan struct{})
	var letGo sync.Once
	fuzzyFiles = func(root string, _ int) ([]string, error) {
		if root == "slow-walk" {
			close(started)
			<-release
		}
		return []string{"x.go"}, nil
	}
	fuzzyLists.mu.Lock()
	fuzzyLists.entries["cached-root"] = fuzzyListing{at: now, paths: []string{"a.go"}}
	fuzzyLists.mu.Unlock()
	t.Cleanup(func() {
		letGo.Do(func() { close(release) })
		<-slowDone
		fuzzyNow = clock
		fuzzyFiles = files
		fuzzyLists.mu.Lock()
		delete(fuzzyLists.entries, "cached-root")
		delete(fuzzyLists.entries, "slow-walk")
		fuzzyLists.mu.Unlock()
	})

	go func() {
		defer close(slowDone)
		_, _ = fuzzyLists.paths("slow-walk")
	}()
	select {
	case <-started:
	case <-time.After(waitLimit):
		t.Fatal("the slow walk did not start")
	}
	hit := make(chan []string, 1)
	go func() {
		paths, _ := fuzzyLists.paths("cached-root")
		hit <- paths
	}()
	select {
	case paths := <-hit:
		if !slices.Equal(paths, []string{"a.go"}) {
			t.Fatalf("cached paths = %v, want [a.go]", paths)
		}
	case <-time.After(waitLimit):
		t.Fatal("a cache hit waited behind an unrelated walk")
	}
}
