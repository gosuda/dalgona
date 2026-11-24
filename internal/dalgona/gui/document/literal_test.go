package document

import (
	"math/rand/v2"
	"reflect"
	"slices"
	"strings"
	"testing"
)

func patchDocument(t *testing.T, item, source string, open bool) *Document {
	t.Helper()
	d, err := New(Header{Identity: committed(t, item), Format: FormatPatch, Open: open}, source)
	if err != nil {
		t.Fatal(err)
	}
	return d
}

func sectionTexts(d *Document) []string {
	texts := make([]string, d.Len())
	for i := range d.Len() {
		b := d.Block(i)
		texts[i] = d.Source()[b.Span.Start:b.Span.End]
	}
	return texts
}

var patchSections = []string{
	"--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-one\n+uno\n two\n",
	"--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+fresh\n",
	"diff --git a/old.go b/new.go\nsimilarity index 90%\nrename from old.go\nrename to new.go\n--- a/old.go\n+++ b/new.go\n@@ -1 +1 @@\n-package old\n+package new\n",
	"diff --git a/logo.png b/logo.png\nindex 1111111..2222222 100644\nBinary files a/logo.png and b/logo.png differ\n",
	"Binary files a/data.bin and b/data.bin differ\n",
	"diff -u a/q.sql b/q.sql\n--- a/q.sql\t2026-01-01\n+++ b/q.sql\t2026-01-02\n@@ -1,2 +1,2 @@\n--- removed comment\n+++ added comment\n keep\n\\ No newline at end of file\n",
	"diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n",
	"--- a/last.txt\r\n+++ b/last.txt\r\n@@ -1 +1 @@\r\n-crlf\r\n+CRLF\r\n",
}

func TestPatchSectionsStartAtEveryFileHeaderForm(t *testing.T) {
	d := patchDocument(t, "patch", strings.Join(patchSections, ""), false)
	if got := sectionTexts(d); !slices.Equal(got, patchSections) {
		t.Fatalf("sections\n%q\nwant\n%q", got, patchSections)
	}
	for i := range d.Len() {
		if d.Block(i).Kind != KindDiff {
			t.Fatalf("section %d kind %d", i, d.Block(i).Kind)
		}
	}
	quoted := patchDocument(t, "plain", "--- a/q.sql\n+++ b/q.sql\n@@ -1,3 +1,3 @@\n--- old\n+++ new\n context\n-x\n+y\n", false)
	if quoted.Len() != 1 {
		t.Fatalf("hunk lines that look like file headers split the patch into %q", sectionTexts(quoted))
	}
}

func TestStreamedPatchConvergesToTheWholePatch(t *testing.T) {
	source := strings.Join(patchSections, "")
	whole := patchDocument(t, "patch", source, false)
	for seed := range uint64(60) {
		r := rand.New(rand.NewPCG(seed, 0xd1ff))
		streamed := appendAll(t, patchDocument(t, "patch", "", true), splitRandomly(r, source)...)
		closed, _, err := streamed.Close()
		if err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(blockShape(closed), blockShape(whole)) {
			t.Fatalf("seed %d: streamed sections %q, want %q", seed, sectionTexts(closed), sectionTexts(whole))
		}
	}
}
