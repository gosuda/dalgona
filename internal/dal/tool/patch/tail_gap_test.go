package patch

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

func TestTailGapUnderReferenceMapsToTheSnapshotEnd(t *testing.T) {
	const bom = "\ufeff"
	tests := []struct {
		rows        string
		name        string
		source      string
		cooperating string
		want        string
	}{
		{"GAP-03 GAP-04", "CUT 1", "one\ntwo\nthree\n", "CUT 1\n", "two\nthree\nfour\n"},
		{"GAP-03 GAP-04", "PUT <1", "one\ntwo\nthree\n", "PUT <1:\n+zero\n", "zero\none\ntwo\nthree\nfour\n"},
		{"GAP-01 GAP-04", "BOM only", bom, "", bom + "four\n"},
		{"GAP-04", "BOM and one unterminated line", bom + "one", "", bom + "onefour\n"},
		{"GAP-04", "BOM and an empty line", bom + "\n", "", bom + "\nfour\n"},
		{"GAP-04", "BOM and CRLF", bom + "\r\n", "", bom + "\r\nfour\r\n"},
		{"GAP-04", "BOM and a CRLF line", bom + "one\r\n", "", bom + "one\r\nfour\r\n"},
		{"GAP-03 GAP-04 E-06", "BOM and PUT <1", bom + "one\ntwo\n", "PUT <1:\n+zero\n", bom + "zero\none\ntwo\nfour\n"},
	}
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		for _, test := range tests {
			t.Run(test.rows+"/"+string(style)+"/"+test.name, func(t *testing.T) {
				requireTailAppend(t, style, test.source, test.cooperating, test.want)
			})
		}
	}
}

func requireTailAppend(t *testing.T, style Style, source, cooperating, want string) {
	t.Helper()
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if cooperating != "" {
		if _, err := runPatch(t, env, style, profileInput("f.txt", ref, cooperating)); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+four\n")); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != want {
		t.Fatalf("tail append %q, want %q", actual, want)
	}
}

func TestDocumentAndSnapshotCountLinesAfterABOM(t *testing.T) {
	const bom = "\ufeff"
	tests := []struct {
		name   string
		source string
		lines  int
	}{
		{"empty", "", 0},
		{"BOM only", bom, 0},
		{"BOM and an empty line", bom + "\n", 1},
		{"BOM and CRLF", bom + "\r\n", 1},
		{"BOM and one unterminated line", bom + "one", 1},
		{"BOM and a terminated line", bom + "one\n", 1},
		{"BOM and two CRLF lines", bom + "one\r\ntwo\r\n", 2},
		{"BOM and an unterminated second line", bom + "one\ntwo", 2},
		{"a second BOM is content", bom + bom, 1},
		{"a BOM after the start is content", "a" + bom, 1},
	}
	for _, test := range tests {
		t.Run("GAP-01 GAP-04/"+test.name, func(t *testing.T) {
			doc, err := decodeDocument("f.txt", []byte(test.source))
			if err != nil {
				t.Fatal(err)
			}
			if doc.lines() != test.lines {
				t.Fatalf("document counts %d lines of %q, want %d", doc.lines(), test.source, test.lines)
			}
			store := snapshot.New()
			ref := store.Put("f.txt", []byte(test.source), nil)
			lines, err := store.Lines(ref)
			if err != nil {
				t.Fatal(err)
			}
			if lines != test.lines {
				t.Fatalf("snapshot counts %d lines of %q, want %d", lines, test.source, test.lines)
			}
		})
	}
}

func TestSecondTailInsertConflictsAfterAnAppend(t *testing.T) {
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		t.Run("GAP-01 E-09/"+string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			source := "one\ntwo\nthree\n"
			writeFixture(t, cwd, "f.txt", source)
			ref := seedReference(env, "f.txt", source, true)
			if _, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+four\n")); err != nil {
				t.Fatal(err)
			}
			_, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+five\n"))
			if err == nil || !strings.HasSuffix(err.Error(), "is stale: snapshot: gap changed. Read again") {
				t.Fatalf("second tail insert error %v", err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != "one\ntwo\nthree\nfour\n" {
				t.Fatalf("conflicting insert wrote %q", actual)
			}
		})
	}
}

func TestSecondTailInsertConflictsAfterAnAppendToABOMOnlyFile(t *testing.T) {
	const source = "\ufeff"
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		t.Run("GAP-01 E-09/"+string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "f.txt", source)
			ref := seedReference(env, "f.txt", source, true)
			if _, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+four\n")); err != nil {
				t.Fatal(err)
			}
			_, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+five\n"))
			if err == nil || !strings.HasSuffix(err.Error(), "is stale: snapshot: gap changed. Read again") {
				t.Fatalf("second tail insert error %v", err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != source+"four\n" {
				t.Fatalf("conflicting insert wrote %q", actual)
			}
		})
	}
}

func TestFinalRowCoverageUsesTheSnapshotRow(t *testing.T) {
	tests := []struct {
		name        string
		cooperating string
		tail        string
	}{
		{"control PUT >3", "", "PUT >3:\n+four\n"},
		{"control PUT >$", "", "PUT >$:\n+four\n"},
		{"CUT 1 then PUT >3", "CUT 1\n", "PUT >3:\n+four\n"},
		{"CUT 1 then PUT >$", "CUT 1\n", "PUT >$:\n+four\n"},
	}
	for _, test := range tests {
		t.Run("E-13/"+test.name, func(t *testing.T) {
			requireFinalRowRefusal(t, test.cooperating, test.tail)
		})
	}
}

func requireFinalRowRefusal(t *testing.T, cooperating, tail string) {
	t.Helper()
	const refusal = "lines 3-3 of f.txt need prior observation. Read them, then resend"
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree"
	writeFixture(t, cwd, "f.txt", source)
	ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 2}})
	want := source
	if cooperating != "" {
		if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, cooperating)); err != nil {
			t.Fatal(err)
		}
		want = "two\nthree"
	}
	_, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, tail))
	if err == nil || !strings.Contains(err.Error(), refusal) {
		t.Fatalf("final row edit error %v, want %q", err, refusal)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != want {
		t.Fatalf("refused edit left %q, want %q", actual, want)
	}
}

func TestFinalRowCoverageAfterPrefixInsertion(t *testing.T) {
	for _, tail := range []string{"PUT >3:\n+four\n", "PUT >$:\n+four\n"} {
		t.Run("E-06 E-13/"+strings.SplitN(tail, ":", 2)[0], func(t *testing.T) {
			env, cwd := editTestEnv(t)
			source := "one\ntwo\nthree"
			writeFixture(t, cwd, "f.txt", source)
			ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 3}})
			if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT <1:\n+zero\n")); err != nil {
				t.Fatal(err)
			}
			if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, tail)); err != nil {
				t.Fatal(err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != "zero\none\ntwo\nthreefour\n" {
				t.Fatalf("append after prefix insertion %q", actual)
			}
		})
	}
}
