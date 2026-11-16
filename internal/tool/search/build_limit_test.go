package search

import (
	"reflect"
	"strings"
	"testing"
	"time"
)

type cappedCase struct {
	name      string
	limit     int64
	unindexed map[string]bool
}

var cappedGrepFiles = map[string]string{
	"a.txt": "needle in a\n",
	"b.txt": "no match here\n",
	"c.txt": "needle in c, past the cap\n",
	"d.txt": "needle in d\n",
}

func TestIndexedGrepOverCappedIndexEqualsScan(t *testing.T) {
	requireChangeTimes(t)
	cases := []cappedCase{
		{name: "capped", limit: 30, unindexed: map[string]bool{"c.txt": true, "d.txt": true}},
		{name: "uncapped", limit: 1 << 30, unindexed: map[string]bool{}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) { requireCappedGrepAgrees(t, tc) })
	}
}

func requireCappedGrepAgrees(t *testing.T, tc cappedCase) {
	t.Helper()
	ws := t.TempDir()
	writeBuildFiles(t, ws, cappedGrepFiles)
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, tc.limit)
	requireRecordedAsExpected(t, readBuildManifest(t, dir), tc)
	query := args{Mode: "grep", Pattern: "needle", Limit: 100}
	scanned, err := execute(ws, query, "", freshness{})
	if err != nil {
		t.Fatal(err)
	}
	indexed, err := execute(ws, query, dir, clockAt(start))
	if err != nil {
		t.Fatal(err)
	}
	if indexed.Text != scanned.Text || !reflect.DeepEqual(indexed.Captures, scanned.Captures) {
		t.Fatalf("indexed output differs from the scan\nindexed: %q\nscan: %q", indexed.Text, scanned.Text)
	}
	requireCappedRows(t, indexed.Text)
	requireCappedCounts(t, scanned, indexed)
}

func requireRecordedAsExpected(t *testing.T, m manifest, tc cappedCase) {
	t.Helper()
	for _, rec := range m.Entries {
		if rec.Indexed == tc.unindexed[rec.Path] {
			t.Fatalf("%s Indexed = %v under limit %d, want %v", rec.Path, rec.Indexed, tc.limit, !tc.unindexed[rec.Path])
		}
	}
}

func requireCappedRows(t *testing.T, text string) {
	t.Helper()
	for _, row := range []string{"a.txt:1: needle in a", "c.txt:1: needle in c, past the cap", "d.txt:1: needle in d"} {
		requireRow(t, text, row)
	}
	if strings.Contains(text, "b.txt") {
		t.Fatalf("output %q names b.txt, which holds no match", text)
	}
}

func requireCappedCounts(t *testing.T, scanned, indexed outcome) {
	t.Helper()
	var total int
	for _, body := range cappedGrepFiles {
		total += len(body)
	}
	if scanned.Unindexed != total {
		t.Fatalf("scan counted %d bytes, want all %d", scanned.Unindexed, total)
	}
	if indexed.Unindexed != 0 {
		t.Fatalf("indexed call counted %d bytes, want 0: a file recorded as not indexed with an unchanged stat does not count", indexed.Unindexed)
	}
}
