package search

import (
	"fmt"
	"maps"
	"math/rand/v2"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"
)

var propertyWords = []string{"a", "b", "c", "k", "K", "\u212a", "\u017f", "ab", "bc", "needle"}

func propertyWord(rng *rand.Rand) string {
	return propertyWords[rng.IntN(len(propertyWords))]
}

func propertyContent(rng *rand.Rand) string {
	var b strings.Builder
	for range rng.IntN(7) {
		for w := range rng.IntN(5) {
			if w > 0 {
				b.WriteByte(' ')
			}
			b.WriteString(propertyWord(rng))
		}
		if rng.IntN(4) == 0 {
			b.WriteByte('\r')
		}
		b.WriteByte('\n')
	}
	return b.String()
}

func propertyFileBody(rng *rand.Rand) string {
	switch rng.IntN(10) {
	case 0:
		return strings.Repeat("k", 2500) + "\n"
	case 1:
		return "\xff" + propertyContent(rng)
	default:
		return propertyContent(rng)
	}
}

func writePropertyTree(t *testing.T, rng *rand.Rand, ws string) []string {
	t.Helper()
	files := make(map[string]string)
	for i := range 1 + rng.IntN(8) {
		files[fmt.Sprintf("d%d/f%d.txt", i%3, i)] = propertyFileBody(rng)
	}
	writeBuildFiles(t, ws, files)
	return slices.Sorted(maps.Keys(files))
}

func sameSize(old []byte, body string) string {
	if len(body) < len(old) {
		return body + strings.Repeat(" ", len(old)-len(body))
	}
	return body[:len(old)]
}

func mutatePropertyFile(t *testing.T, rng *rand.Rand, ws, name string) {
	t.Helper()
	path := filepath.Join(ws, filepath.FromSlash(name))
	switch rng.IntN(6) {
	case 0:
		mutateSameSize(t, rng, ws, name)
	case 1:
		writeBuildFiles(t, ws, map[string]string{name: propertyContent(rng)})
	case 2:
		removePropertyFile(t, path)
	case 3:
		renamePropertyFile(t, path)
	case 4:
		mutateWithRestoredMtime(t, rng, ws, name)
	}
}

func mutateSameSize(t *testing.T, rng *rand.Rand, ws, name string) {
	t.Helper()
	path := filepath.Join(ws, filepath.FromSlash(name))
	old, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	writeBuildFiles(t, ws, map[string]string{name: sameSize(old, propertyContent(rng))})
}

func removePropertyFile(t *testing.T, path string) {
	t.Helper()
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
}

func renamePropertyFile(t *testing.T, path string) {
	t.Helper()
	if err := os.Rename(path, path+".moved"); err != nil {
		t.Fatal(err)
	}
}

func mutateWithRestoredMtime(t *testing.T, rng *rand.Rand, ws, name string) {
	t.Helper()
	path := filepath.Join(ws, filepath.FromSlash(name))
	before, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	mutateSameSize(t, rng, ws, name)
	if err := os.Chtimes(path, before.ModTime(), before.ModTime()); err != nil {
		t.Fatal(err)
	}
}

func mutatePropertyTree(t *testing.T, rng *rand.Rand, ws string, names []string) {
	t.Helper()
	for _, name := range names {
		mutatePropertyFile(t, rng, ws, name)
		if rng.IntN(len(names)) == 0 {
			writeBuildFiles(t, ws, map[string]string{name: propertyContent(rng)})
		}
	}
	writeBuildFiles(t, ws, map[string]string{"racy/new.txt": propertyContent(rng)})
}

func propertyArgs(rng *rand.Rand) args {
	var pattern string
	switch rng.IntN(5) {
	case 0:
		words := make([]string, 1+rng.IntN(4))
		for i := range words {
			words[i] = propertyWord(rng)
		}
		pattern = strings.Join(words, " ")
	case 1:
		pattern = propertyWord(rng) + "|" + propertyWord(rng)
	case 2:
		pattern = "."
	case 3:
		pattern = "[a-c]+"
	default:
		pattern = "(?i)" + propertyWord(rng)
	}
	return args{Mode: "grep", Pattern: pattern, Literal: rng.IntN(2) == 0, IgnoreCase: rng.IntN(2) == 0, Limit: 100}
}

func TestIndexedGrepEqualsScan(t *testing.T) {
	requireChangeTimes(t)
	rng := rand.New(rand.NewPCG(20261112, 1))
	for i := range 200 {
		ws := t.TempDir()
		names := writePropertyTree(t, rng, ws)
		start := time.Now()
		switch rng.IntN(3) {
		case 1:
			start = start.Add(time.Hour)
		case 2:
			start = start.Add(-time.Hour)
		}
		dir := buildTestIndex(t, ws, start, 1<<30)
		mutatePropertyTree(t, rng, ws, names)
		clock := clockAt(start)
		if rng.IntN(4) == 0 {
			clock = freshness{Now: time.Now().UnixNano(), Racy: racyWindow.Nanoseconds()}
		}
		c := indexedCase{caseNo: i, ws: ws, dir: dir, clock: clock}
		for range 8 {
			requireIndexedAgrees(t, c, propertyArgs(rng))
		}
	}
}

type indexedCase struct {
	caseNo int
	ws     string
	dir    string
	clock  freshness
}

func requireIndexedAgrees(t *testing.T, c indexedCase, query args) outcome {
	t.Helper()
	scanned, err := execute(c.ws, query, "", freshness{})
	if err != nil {
		t.Fatal(err)
	}
	indexed, err := execute(c.ws, query, c.dir, c.clock)
	if err != nil {
		t.Fatal(err)
	}
	if scanned.Text != indexed.Text || !reflect.DeepEqual(scanned.Captures, indexed.Captures) {
		t.Fatalf("case %d pattern %q literal %v ignore-case %v: indexed output differs from the scan\nindexed: %q\nscan: %q",
			c.caseNo, query.Pattern, query.Literal, query.IgnoreCase, indexed.Text, scanned.Text)
	}
	if indexed.Unindexed > scanned.Unindexed {
		t.Fatalf("case %d: indexed Unindexed %d exceeds the scan's %d", c.caseNo, indexed.Unindexed, scanned.Unindexed)
	}
	return scanned
}

func propertyLongLine(rng *rand.Rand, size int) string {
	var block strings.Builder
	for block.Len() < 1<<10 {
		block.WriteString(propertyFiller(rng))
		block.WriteByte(' ')
	}
	filler := func(atLeast int) string {
		return strings.Repeat(block.String(), (atLeast+block.Len()-1)/block.Len())
	}
	return filler(64<<10) + "midmark " + filler(size-64<<10-block.Len()) + "endmark"
}

func propertyFiller(rng *rand.Rand) string {
	for {
		if word := propertyWord(rng); word != "needle" {
			return word
		}
	}
}

type longLineQuery struct {
	args    args
	longRow bool
}

func TestIndexedGrepEqualsScanOnLongLines(t *testing.T) {
	requireChangeTimes(t)
	rng := rand.New(rand.NewPCG(20261112, 2))
	search := func(pattern string, literal, ignoreCase bool) args {
		return args{Mode: "grep", Pattern: pattern, Literal: literal, IgnoreCase: ignoreCase, Limit: 100}
	}
	mid := longLineQuery{search("midmark", true, false), true}
	end := longLineQuery{search("endmark", true, false), true}
	absent := longLineQuery{search("needle", true, false), false}
	either := longLineQuery{search("midmark|endmark", false, false), true}
	folded := longLineQuery{search("MIDMARK", true, true), true}
	sizes := []struct {
		bytes   int
		queries []longLineQuery
	}{
		{1 << 20, []longLineQuery{mid, end, absent, either, folded}},
		{9 << 20, []longLineQuery{end}},
	}
	for _, size := range sizes {
		t.Run(fmt.Sprintf("%dMiB", size.bytes>>20), func(t *testing.T) {
			ws := t.TempDir()
			writeBuildFiles(t, ws, map[string]string{
				"long/f.txt":  "head\n" + propertyLongLine(rng, size.bytes) + "\ntail needle\n",
				"short/g.txt": "needle\nmidmark here\nendmark here\n",
			})
			start := time.Now().Add(time.Hour)
			c := indexedCase{ws: ws, dir: buildTestIndex(t, ws, start, 1<<30), clock: clockAt(start)}
			for _, query := range size.queries {
				out := requireIndexedAgrees(t, c, query.args)
				if got := strings.Contains(out.Text, "long/f.txt:2: "); got != query.longRow {
					t.Fatalf("pattern %q: hit row for the long line is %v, want %v, in %q", query.args.Pattern, got, query.longRow, out.Text)
				}
			}
		})
	}
}

func TestIndexedGrepHandWrittenRows(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "one needle\ntwo\nneedle again\n"})
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	out := indexedGrep(t, ws, dir, clockAt(start), "needle")
	requireRow(t, out.Text, "a.txt:1: one needle")
	requireRow(t, out.Text, "a.txt:3: needle again")
}
