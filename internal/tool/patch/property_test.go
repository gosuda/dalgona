package patch

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io/fs"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func editTestEnv(t *testing.T) (tool.Env, string) {
	t.Helper()
	cwd := t.TempDir()
	local, err := runner.NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	return tool.Env{Runner: local, Cwd: cwd, Home: t.TempDir(), Snapshots: snapshot.New()}, cwd
}

func writeFixture(t *testing.T, cwd, name, body string) {
	t.Helper()
	path := filepath.Join(cwd, name)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func readFixture(t *testing.T, cwd, name string) string {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(cwd, name))
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

func runPatch(t *testing.T, env tool.Env, style Style, input string) (tool.Result, error) {
	t.Helper()
	args := []byte(input)
	if style != Replace {
		args = []byte(`{"input":` + snapshot.JSONString(input) + `}`)
	}
	return New(string(style)).Run(t.Context(), tool.Call{ID: "call", Args: args}, env)
}

func seedReference(env tool.Env, path, body string, shown bool) string {
	var ranges []snapshot.Range
	if shown {
		lines := countReferenceLines(body)
		if lines > 0 {
			ranges = []snapshot.Range{{Start: 1, End: lines}}
		}
	}
	return env.Snapshots.Put(path, []byte(body), ranges)
}

func countReferenceLines(body string) int {
	if body == "" {
		return 0
	}
	count := strings.Count(body, "\n")
	if !strings.HasSuffix(body, "\n") {
		count++
	}
	return count
}

type referenceEdit struct {
	start int
	end   int
	body  []string
}

func referenceApply(lines []string, edits []referenceEdit) []string {
	out := slices.Clone(lines)
	for i := len(edits) - 1; i >= 0; i-- {
		item := edits[i]
		next := make([]string, 0, len(out)-(item.end-item.start)+len(item.body))
		next = append(next, out[:item.start]...)
		next = append(next, item.body...)
		next = append(next, out[item.end:]...)
		out = next
	}
	return out
}

func randomEdits(rng *rand.Rand, lines int) []referenceEdit {
	count := 1 + rng.IntN(min(3, lines))
	permutation := rng.Perm(lines)
	out := make([]referenceEdit, 0, count)
	for _, start := range permutation[:count] {
		body := []string{fmt.Sprintf("new-%d-%d", start, rng.IntN(10000))}
		if rng.IntN(2) == 0 {
			body = append(body, fmt.Sprintf("extra-%d", rng.IntN(10000)))
		}
		out = append(out, referenceEdit{start: start, end: start + 1, body: body})
	}
	slicesSort(out)
	for i := range out {
		next := lines
		if i+1 < len(out) {
			next = out[i+1].start
		}
		if out[i].end < next && rng.IntN(2) == 0 {
			out[i].end++
		}
	}
	return out
}

func slicesSort(items []referenceEdit) {
	slices.SortFunc(items, func(a, b referenceEdit) int { return cmp.Compare(a.start, b.start) })
}

type styleProof struct {
	tag string
	ref string
}

func styleProofFor(env tool.Env, style Style, source string) styleProof {
	proof := styleProof{tag: snapshot.Tag("whole", []byte(source))}
	switch style {
	case Hashline:
		proof.ref = snapshot.VersionTag([]byte(source))
	case HashlineLight, HashlineEnhanced:
		proof.ref = seedReference(env, "f.txt", source, true)
	case Anchor:
		seedReference(env, "f.txt", source, true)
	case Replace, ApplyPatch:
	}
	return proof
}

func renderStyle(style Style, source []string, edits []referenceEdit, proof styleProof) string {
	switch style {
	case Replace:
		return renderReplace("f.txt", source, edits)
	case Anchor:
		return renderAnchor("f.txt", source, edits, proof.tag)
	case Hashline, HashlineLight, HashlineEnhanced:
		return renderHashline("f.txt", edits, proof.ref)
	case ApplyPatch:
		return renderApplyPatch("f.txt", source, edits)
	}
	panic("unknown style")
}

func bodyLines(lines []string) string {
	return strings.Join(lines, "\n") + "\n"
}

func renderReplace(path string, source []string, edits []referenceEdit) string {
	var out strings.Builder
	out.WriteString(`{"changes":[`)
	for i, item := range edits {
		if i > 0 {
			out.WriteByte(',')
		}
		old := bodyLines(source[item.start:item.end])
		fmt.Fprintf(&out, `{"path":%s,"old":%s,"new":%s}`, snapshot.JSONString(path), snapshot.JSONString(old), snapshot.JSONString(bodyLines(item.body)))
	}
	out.WriteString(`]}`)
	return out.String()
}

func renderAnchor(path string, source []string, edits []referenceEdit, tag string) string {
	var out strings.Builder
	fmt.Fprintf(&out, "*** File: %s #%s\n", path, tag)
	for _, item := range edits {
		fmt.Fprintf(&out, "*** Find %d-%d\n%s*** Replace\n%s", item.start+1, item.end, bodyLines(source[item.start:item.end]), bodyLines(item.body))
	}
	return out.String()
}

func renderHashline(path string, edits []referenceEdit, proof string) string {
	var out strings.Builder
	separator := "#"
	if strings.HasPrefix(proof, "r") {
		separator = "@"
	}
	fmt.Fprintf(&out, "[%s%s%s]\n", path, separator, proof)
	for _, item := range edits {
		fmt.Fprintf(&out, "PUT %d-%d:\n", item.start+1, item.end)
		for _, line := range item.body {
			out.WriteString("+" + line + "\n")
		}
	}
	return out.String()
}

func renderApplyPatch(path string, source []string, edits []referenceEdit) string {
	var out strings.Builder
	out.WriteString("*** Begin Patch\n*** Update File: " + path + "\n")
	for _, item := range edits {
		out.WriteString("@@\n")
		for _, line := range source[item.start:item.end] {
			out.WriteString("-" + line + "\n")
		}
		for _, line := range item.body {
			out.WriteString("+" + line + "\n")
		}
	}
	out.WriteString("*** End Patch\n")
	return out.String()
}

func TestStyleProperties(t *testing.T) {
	for _, style := range []Style{Anchor, Replace, Hashline, HashlineLight, HashlineEnhanced, ApplyPatch} {
		t.Run(string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			rng := rand.New(rand.NewPCG(100, uint64(len(style))))
			for range 60 {
				checkStyleIteration(t, env, cwd, style, rng)
			}
		})
	}
}

func checkStyleIteration(t *testing.T, env tool.Env, cwd string, style Style, rng *rand.Rand) {
	t.Helper()
	lines := make([]string, 4+rng.IntN(7))
	for i := range lines {
		lines[i] = fmt.Sprintf("line-%d-%d", rng.IntN(1<<20), i)
	}
	edits := randomEdits(rng, len(lines))
	source := bodyLines(lines)
	writeFixture(t, cwd, "f.txt", source)
	input := renderStyle(style, lines, edits, styleProofFor(env, style, source))
	plan, err := New(string(style)).Plan(t.Context(), tool.Call{ID: "property", Args: styleArgs(style, input)}, env)
	if err != nil {
		t.Fatalf("plan: %v\n%s", err, input)
	}
	if len(plan.Files) != 1 || plan.Files[0].PreSHA != snapshot.Digest([]byte(source)) {
		t.Fatalf("bad pre-image plan: %+v", plan.Files)
	}
	if result, applyErr := plan.Apply(t.Context()); applyErr != nil || !result.OK {
		t.Fatalf("apply: %v %+v", applyErr, result)
	}
	expected := bodyLines(referenceApply(lines, edits))
	if actual := readFixture(t, cwd, "f.txt"); actual != expected {
		t.Fatalf("actual:\n%s\nexpected:\n%s\ninput:\n%s", actual, expected, input)
	}
	if plan.Files[0].PostSHA != snapshot.Digest([]byte(expected)) {
		t.Fatal("bad post sha")
	}
}

func styleArgs(style Style, input string) jsontext.Value {
	if style == Replace {
		return jsontext.Value(input)
	}
	return jsontext.Value(`{"input":` + snapshot.JSONString(input) + `}`)
}

func TestStaleProofsRejected(t *testing.T) {
	for _, style := range []Style{Anchor, Replace, Hashline, HashlineLight, HashlineEnhanced} {
		t.Run(string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			source := "one\ntwo\n"
			writeFixture(t, cwd, "f.txt", source)
			proof := styleProofFor(env, style, source)
			writeFixture(t, cwd, "f.txt", "drift\n")
			input := renderStyle(style, []string{"one", "two"}, []referenceEdit{{start: 0, end: 1, body: []string{"new"}}}, proof)
			if _, err := runPatch(t, env, style, input); err == nil {
				t.Fatal("stale edit succeeded")
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != "drift\n" {
				t.Fatalf("stale edit wrote %q", actual)
			}
		})
	}
}

func TestParseErrorSuffix(t *testing.T) {
	for _, style := range []Style{Anchor, Replace, Hashline, HashlineLight, HashlineEnhanced, ApplyPatch} {
		_, err := runPatch(t, tool.Env{}, style, "junk")
		if err == nil {
			t.Fatalf("%s accepted junk", style)
		}
		suffix := fmt.Sprintf("\nCurrent edit_style is %q for this model: past calls in history may use another dialect. Do not imitate them.", style)
		if !strings.HasSuffix(err.Error(), suffix) {
			t.Fatalf("%s error %q lacks suffix", style, err)
		}
	}
}

type failOnceRunner struct {
	runner.Runner
	target string
	failed bool
}

func (r *failOnceRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if path == r.target && !r.failed {
		r.failed = true
		return errors.New("injected publish failure")
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

func TestSecondOfThreeFailureRestoresFirst(t *testing.T) {
	env, cwd := editTestEnv(t)
	for _, name := range []string{"a.txt", "b.txt", "c.txt"} {
		writeFixture(t, cwd, name, name+"-before\n")
	}
	failing := &failOnceRunner{Runner: env.Runner, target: filepath.Join(cwd, "b.txt")}
	env.Runner = failing
	input := `{"changes":[{"path":"a.txt","old":"a.txt-before","new":"a-after"},{"path":"b.txt","old":"b.txt-before","new":"b-after"},{"path":"c.txt","old":"c.txt-before","new":"c-after"}]}`
	_, err := runPatch(t, env, Replace, input)
	if err == nil {
		t.Fatal("injected failure succeeded")
	}
	for _, name := range []string{"a.txt", "b.txt", "c.txt"} {
		if actual := readFixture(t, cwd, name); actual != name+"-before\n" {
			t.Fatalf("%s was not restored: %q", name, actual)
		}
		if !strings.Contains(err.Error(), name) {
			t.Fatalf("error does not name %s: %v", name, err)
		}
	}
}

func TestMaskerUnmasksLocatorAndBody(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "old-secret\n")
	env.Masker = maskerStub{}
	input := `{"changes":[{"path":"f.txt","old":"old-{{secret}}","new":"new-{{secret}}"}]}`
	if _, err := runPatch(t, env, Replace, input); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "new-secret\n" {
		t.Fatalf("masking result %q", actual)
	}
}

func TestMaskerLeavesPathsAndPreservesJSON(t *testing.T) {
	env, cwd := editTestEnv(t)
	path := "literal-{{secret}}.txt"
	masked := "a&b'c #d]e"
	writeFixture(t, cwd, path, masked)
	env.Masker = quoteMasker{secret: masked}
	input := `{"changes":[{"path":` + snapshot.JSONString(path) + `,"old":"{{secret}}","new":"new-{{secret}}"}]}`
	if _, err := runPatch(t, env, Replace, input); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, path); actual != "new-"+masked {
		t.Fatalf("masked quoted secret result %q", actual)
	}
	if _, err := os.Stat(filepath.Join(cwd, "literal-"+masked+".txt")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatal("path text was unmasked")
	}
	checkHostileMaskerRoundTrip(t)
}

func checkHostileMaskerRoundTrip(t *testing.T) {
	t.Helper()
	hostileSecret := "a\"b\\c\n"
	encoded := snapshot.JSONString(hostileSecret)
	decoded, decodeErr := jsonString(jsontext.Value(encoded))
	if decodeErr != nil {
		t.Fatal(decodeErr)
	}
	if decoded != hostileSecret {
		t.Fatalf("hostile JSON %q, want %q", decoded, hostileSecret)
	}
	hostileInput := `{"changes":[{"path":` + snapshot.JSONString("literal-{{secret}}.txt") + `,"old":"{{secret}}","new":"new-{{secret}}"}]}`
	hostileEdits, parseErr := parse(Replace, hostileInput)
	if parseErr != nil {
		t.Fatal(parseErr)
	}
	if len(hostileEdits) != 1 {
		t.Fatalf("hostile edits %d, want 1", len(hostileEdits))
	}
	if unmaskErr := unmaskEdits(hostileEdits, quoteMasker{secret: hostileSecret}.UnmaskStrict); unmaskErr != nil {
		t.Fatal(unmaskErr)
	}
	if hostileEdits[0].path != "literal-{{secret}}.txt" {
		t.Fatalf("hostile path %q was unmasked", hostileEdits[0].path)
	}
	if hostileEdits[0].locator.old != hostileSecret {
		t.Fatalf("hostile old %q, want %q", hostileEdits[0].locator.old, hostileSecret)
	}
	if hostileEdits[0].body != "new-"+hostileSecret {
		t.Fatalf("hostile new %q, want %q", hostileEdits[0].body, "new-"+hostileSecret)
	}
}

func TestStyleAliasesAndDefault(t *testing.T) {
	for name, want := range map[string]Style{
		"": Anchor, "balanced": Anchor, "simple": Replace, "strict": Hashline,
	} {
		if got := canonicalStyle(name); got != want {
			t.Fatalf("style %q resolved to %q, want %q", name, got, want)
		}
	}
}

func TestPatchRejectsUnsafePaths(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "body\n")
	absolute := filepath.Join(cwd, "outside.txt")
	payloads := []string{
		`{"changes":[{"path":` + snapshot.JSONString(absolute) + `,"create":"x"}]}`,
		`{"changes":[{"path":"bad\u0000x","create":"x"}]}`,
		`{"changes":[{"path":"../escape.txt","create":"x"}]}`,
		`{"changes":[{"path":"f.txt","rename":"a/../outside.txt"}]}`,
	}
	for _, input := range payloads {
		if _, err := runPatch(t, env, Replace, input); err == nil {
			t.Fatalf("accepted unsafe patch %s", input)
		}
	}
	if _, err := os.Stat(absolute); !errors.Is(err, fs.ErrNotExist) {
		t.Fatal("unsafe patch wrote an absolute path")
	}
}

func TestAnchorSpanQuoteMustMatchLines(t *testing.T) {
	source := "one\ntwo\nthree\nfour\n"
	cases := []struct {
		name  string
		input string
		want  string
	}{
		{"boundary", "*** File: f.txt\n*** Find 2-4\ntwo\nfour\n*** Replace\nX\n", "one\nX\n"},
		{"every_line", "*** File: f.txt\n*** Find 2-3\ntwo\nthree\n*** Replace\nX\n", "one\nX\nfour\n"},
		{"wrong_lines", "*** File: f.txt\n*** Find 2-3\none\ntwo\n*** Replace\nX\n", source},
		{"wrong_count", "*** File: f.txt\n*** Find 1-4\none\ntwo\nthree\n*** Replace\nX\n", source},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "f.txt", source)
			seedReference(env, "f.txt", source, true)
			_, err := runPatch(t, env, Anchor, test.input)
			if (err == nil) != (test.want != source) {
				t.Fatalf("span result error %v", err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != test.want {
				t.Fatalf("span result %q, want %q", actual, test.want)
			}
		})
	}
}

func TestOutOfRangeLineEditsWriteNothing(t *testing.T) {
	source := "one\ntwo\n"
	inputs := map[Style]string{
		Hashline:      "[f.txt#" + snapshot.VersionTag([]byte(source)) + "]\nPUT 3:\n+x\n",
		HashlineLight: "PUT <9:\n+x\n",
		Anchor:        "*** File: f.txt\n*** Find 2-3\ntwo\nthree\n*** Replace\nx\n",
	}
	for style, input := range inputs {
		t.Run(string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "f.txt", source)
			ref := seedReference(env, "f.txt", source, true)
			if style == HashlineLight {
				input = profileInput("f.txt", ref, input)
			}
			if _, err := runPatch(t, env, style, input); err == nil {
				t.Fatal("out-of-range edit succeeded")
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != source {
				t.Fatalf("out-of-range edit wrote %q", actual)
			}
		})
	}
}

func TestReplaceAllNeedsWholeFileProof(t *testing.T) {
	source := "a\nb\na\n"
	tag := snapshot.Tag("whole", []byte(source))
	cases := []struct {
		name  string
		input string
		want  string
	}{
		{"tag", `{"changes":[{"path":"f.txt","old":"a","new":"z","all":true,"tag":"` + tag + `"}]}`, "z\nb\nz\n"},
		{"stale_tag", `{"changes":[{"path":"f.txt","old":"a","new":"z","all":true,"tag":"00000000"}]}`, source},
		{"tag_without_all", `{"changes":[{"path":"f.txt","old":"a","new":"z","tag":"` + tag + `"}]}`, source},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "f.txt", source)
			_, err := runPatch(t, env, Replace, test.input)
			if (err == nil) != (test.want != source) {
				t.Fatalf("replace all error %v", err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != test.want {
				t.Fatalf("replace all %q, want %q", actual, test.want)
			}
		})
	}
}

type quoteMasker struct {
	secret string
}

func (m quoteMasker) Mask(text string) string {
	return strings.ReplaceAll(text, m.secret, "{{secret}}")
}

func (m quoteMasker) Unmask(text string) string {
	return strings.ReplaceAll(text, "{{secret}}", m.secret)
}

func (m quoteMasker) UnmaskStrict(text string) (string, error) {
	return m.Unmask(text), nil
}

type maskerStub struct{}

func (maskerStub) Mask(text string) string { return strings.ReplaceAll(text, "secret", "{{secret}}") }

func (maskerStub) Unmask(text string) string { return strings.ReplaceAll(text, "{{secret}}", "secret") }

func (maskerStub) UnmaskStrict(text string) (string, error) {
	return maskerStub{}.Unmask(text), nil
}
