package patch

import (
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func nonOverlappingRanges(view, old string) string {
	if old == "" {
		return ""
	}
	var parts []string
	for offset := 0; offset <= len(view)-len(old); {
		at := strings.Index(view[offset:], old)
		if at < 0 {
			break
		}
		start := offset + at
		parts = append(parts, "["+strconv.Itoa(start)+" "+strconv.Itoa(start+len(old))+")")
		offset = start + len(old)
	}
	return strings.Join(parts, " ")
}

func TestFindTextMatchesYieldsNonOverlappingLeftmostMatches(t *testing.T) {
	cases := []struct {
		view, old string
	}{
		{"----\n", "--"},
		{"aaaa", "aa"},
		{"abababa", "aba"},
		{"aa\naa\naa\n", "aa\naa\n"},
		{"abc", "xyz"},
		{"a-na-na", "a-na-na"},
	}
	for _, tc := range cases {
		matches := findTextMatches(tc.view, tc.old, "", false)
		parts := make([]string, 0, len(matches))
		for _, match := range matches {
			parts = append(parts, "["+strconv.Itoa(match.start)+" "+strconv.Itoa(match.end)+")")
		}
		if actual := strings.Join(parts, " "); actual != nonOverlappingRanges(tc.view, tc.old) {
			t.Errorf("findTextMatches(%q, %q) = %q, want strings.ReplaceAll offsets %q", tc.view, tc.old, actual, nonOverlappingRanges(tc.view, tc.old))
		}
	}
}

func TestReplaceAllSplicesNonOverlappingMatches(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "dashes.txt", "----\n")
	tag := snapshot.Tag("whole", []byte("----\n"))
	result, err := runPatch(t, env, Replace, `{"changes":[{"path":"dashes.txt","old":"--","new":"-","all":true,"tag":"`+tag+`"}]}`)
	if err != nil {
		t.Fatalf("replace all failed: %v", err)
	}
	if !result.OK {
		t.Fatalf("replace all not ok: %s", result.Output)
	}
	if actual := readFixture(t, cwd, "dashes.txt"); actual != "--\n" {
		t.Fatalf("self-overlapping replace all wrote %q, want %q per strings.ReplaceAll", actual, "--\n")
	}
}

func TestAnchorFindAllSplicesNonOverlappingMatches(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "aa\naa\naa\n"
	writeFixture(t, cwd, "anchor.txt", source)
	tag := snapshot.Tag("whole", []byte(source))
	input := "*** File: anchor.txt #" + tag + "\n*** Find all\naa\naa\n*** Replace\nX\n"
	if _, err := runPatch(t, env, Anchor, input); err != nil {
		t.Fatalf("anchor find all failed: %v", err)
	}
	if actual := readFixture(t, cwd, "anchor.txt"); actual != "X\naa\n" {
		t.Fatalf("self-overlapping anchor find all wrote %q, want %q per strings.ReplaceAll", actual, "X\naa\n")
	}
}

func TestAmbiguousReplaceCountsNonOverlappingOccurrences(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "quad.txt", "aaaa\n")
	_, err := runPatch(t, env, Replace, `{"changes":[{"path":"quad.txt","old":"aa","new":"X"}]}`)
	if err == nil {
		t.Fatal("ambiguous replace succeeded")
	}
	if !strings.Contains(err.Error(), "occurs 2 times") {
		t.Fatalf("error %v, want the non-overlapping occurrence count 2", err)
	}
	if actual := readFixture(t, cwd, "quad.txt"); actual != "aaaa\n" {
		t.Fatalf("failed patch changed the file to %q", actual)
	}
}
