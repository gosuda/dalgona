package search

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/syntax"
)

func TestReferencesRows(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", "package p\ntype Router struct{}\nfunc (r Router) Handle() { peer() }\nvar _ = \"peer\"\n// peer\n")
	writeSearchFixture(t, root, "b.ts", "function work() { peer(); }\nconst text = 'peer';\n// peer\n")
	result := runMain(t, `{"mode":"references","pattern":"peer"}`)
	want := []string{"a.go:3: call peer in Router.Handle by name", "b.ts:1: call peer in work by name"}
	if got := strings.Split(result.Text, "\n"); !slices.Equal(got, want) {
		t.Fatalf("rows = %q, want %q", got, want)
	}
	if len(result.Captures) != 0 {
		t.Fatal("references carried snapshots")
	}
	grep := runMain(t, `{"mode":"grep","pattern":"peer","path":"b.ts"}`)
	if !strings.Contains(grep.Text, "b.ts:2:") || !strings.Contains(grep.Text, "b.ts:3:") {
		t.Fatalf("grep did not find string and comment: %s", grep.Text)
	}
	limited := runMain(t, `{"mode":"references","pattern":"peer","glob":"*.go","limit":1}`)
	if limited.Text != want[0] {
		t.Fatalf("glob result = %q", limited.Text)
	}
	cut := runMain(t, `{"mode":"references","pattern":"peer","limit":1}`)
	if !strings.HasSuffix(cut.Text, "[Truncated: 1 more rows]") {
		t.Fatalf("cut result = %q", cut.Text)
	}
}

func TestImportersMatchBoundary(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	specs := []string{"github.com/a/b/syntax", "github.com/a/b/syntaxx", "syntax", "a.syntax"}
	for i, spec := range specs {
		writeSearchFixture(t, root, fmt.Sprintf("%d.go", i), fmt.Sprintf("package p\nimport %q\n", spec))
	}
	writeSearchFixture(t, root, "nested.rs", "use a::{syntax, other};\n")
	result := runMain(t, `{"mode":"importers","pattern":"syntax"}`)
	want := []string{"0.go:2: import github.com/a/b/syntax by name", "2.go:2: import syntax by name", "3.go:2: import a.syntax by name", "nested.rs:1: import a::syntax by name"}
	if got := strings.Split(result.Text, "\n"); !slices.Equal(got, want) {
		t.Fatalf("rows = %q, want %q", got, want)
	}
	result = runMain(t, `{"mode":"importers","pattern":"a::syntax"}`)
	if result.Text != want[3] {
		t.Fatalf("normalized import was prefiltered: %q", result.Text)
	}
}

func TestGraphModeRefusals(t *testing.T) {
	t.Parallel()
	cases := []struct{ raw, want string }{
		{`{"mode":"references","pattern":"Server.Run"}`, "search: references takes one bare name of letters, digits, _ and $. Write Run, not Server.Run"},
		{`{"mode":"importers","pattern":"a b"}`, "search: importers takes one import path without spaces"},
		{`{"mode":"references","pattern":"x","literal":true}`, "search: literal and ignore_case do not apply to references"},
		{`{"mode":"importers","pattern":"x","ignore_case":true}`, "search: literal and ignore_case do not apply to importers"},
		{`{"mode":"references"}`, "search: pattern must not be empty"},
		{`{"mode":"importers"}`, "search: pattern must not be empty"},
		{`{"mode":"references","pattern":"x","kind":["method"]}`, "search: kind applies only to symbols and definition"},
		{`{"mode":"importers","pattern":"x","lang":"go"}`, "search: lang applies only to syntax mode"},
	}
	for _, test := range cases {
		_, err := parseArgs(jsontext.Value(test.raw))
		if err == nil || err.Error() != test.want {
			t.Fatalf("%s: got %v, want %s", test.raw, err, test.want)
		}
	}
	for _, raw := range []string{`{"mode":"references","pattern":"名前"}`, `{"mode":"importers","pattern":"a::b"}`} {
		if _, err := parseArgs(jsontext.Value(raw)); err != nil {
			t.Fatalf("valid arguments %s: %v", raw, err)
		}
	}
}

func graphFixtureView(t *testing.T, bodies []string) *graphView {
	t.Helper()
	root := t.TempDir()
	var entries []fileEntry
	for i, body := range bodies {
		name := fmt.Sprintf("%03d.go", i)
		writeSearchFixture(t, root, name, body)
		entries = append(entries, fileEntry{absolute: filepath.Join(root, name), display: name})
	}
	return newGraphView(graphCall{entries: entries, args: args{Mode: "references", Pattern: "peer", Limit: 1000}, start: time.Now(), now: time.Now})
}

func TestGraphTraversalSharesBudgets(t *testing.T) {
	t.Parallel()
	bodies := make([]string, parseMaxFiles+1)
	for i := range bodies {
		bodies[i] = "package p\nfunc f() { peer() }\n"
	}
	view := graphFixtureView(t, bodies)
	if err := view.load(t.Context(), []string{"peer"}); err != nil {
		t.Fatal(err)
	}
	if view.parsedFiles != parseMaxFiles || view.tally.notParsed != 1 {
		t.Fatalf("parsed %d, refused %d", view.parsedFiles, view.tally.notParsed)
	}
	if err := view.load(t.Context(), nil); err != nil {
		t.Fatal(err)
	}
	if view.parsedFiles != parseMaxFiles || view.tally.notParsed != 1 {
		t.Fatal("repeat load reset the file budget or counted a refusal twice")
	}
}

func TestGraphTraversalByteAndFileBounds(t *testing.T) {
	t.Parallel()
	body := "package p\n" + strings.Repeat(" ", parseMaxFileBytes-len("package p\n"))
	view := graphFixtureView(t, []string{body, body, body, body, "package p\n"})
	jobs, err := view.admit(t.Context(), nil)
	if err != nil || len(jobs) != 4 || view.parsedBytes != parseMaxBytes || view.tally.notParsed != 1 {
		t.Fatalf("jobs %d, bytes %d, refused %d, error %v", len(jobs), view.parsedBytes, view.tally.notParsed, err)
	}
	view = graphFixtureView(t, []string{body + " ", "package p\n"})
	jobs, err = view.admit(t.Context(), nil)
	if err != nil || len(jobs) != 1 || view.tally.notParsed != 1 {
		t.Fatalf("per-file cap: jobs %d, refused %d, error %v", len(jobs), view.tally.notParsed, err)
	}
}

func TestGraphTraversalNeedlesAndAdmissionTime(t *testing.T) {
	t.Parallel()
	view := graphFixtureView(t, []string{"package p\nfunc f() { first() }\n", "package p\nfunc g() { second() }\n"})
	if err := view.load(t.Context(), []string{"first"}); err != nil {
		t.Fatal(err)
	}
	if view.attempted["001.go"] {
		t.Fatal("needle miss marked attempted")
	}
	if err := view.load(t.Context(), []string{"second"}); err != nil {
		t.Fatal(err)
	}
	if len(view.callSites([]string{"second"}, syntax.RefCall)) != 1 {
		t.Fatal("later needle did not reach skipped source")
	}
	view = graphFixtureView(t, []string{"package p\n", "package p\n"})
	requireDeadlineTally(t, view)
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	_, err := graphMode(ctx, view.call)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled graph call = %v", err)
	}
}

func requireDeadlineTally(t *testing.T, view *graphView) {
	t.Helper()
	view.call.now = func() time.Time { return view.call.start.Add(parseBudget) }
	for range 2 {
		if err := view.load(t.Context(), nil); err != nil {
			t.Fatal(err)
		}
	}
	if view.tally.stopped != 2 || view.parsedFiles != 0 {
		t.Fatalf("deadline tally: %+v, parsed %d", view.tally, view.parsedFiles)
	}
}

func TestGraphNoGraphFooter(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.js", "peer()\n")
	writeSearchFixture(t, root, "b.css", "body {}\n")
	writeSearchFixture(t, root, "c.js", "peer()\n")
	writeSearchFixture(t, root, "d.go", "package p\n")
	result := runMain(t, `{"mode":"references","pattern":"peer"}`)
	want := "No matches for peer by name. Searched 1 files of languages with a graph.\n[No graph: 3 files of css, javascript. Use grep.]"
	if result.Text != want {
		t.Fatalf("result = %q, want %q", result.Text, want)
	}
}
