package stdlib_test

import (
	"bytes"
	"errors"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/script/stdlib"
	"go.starlark.net/starlark"
)

type oracleCase struct {
	name   string
	python string
	star   string
}

func TestPredeclared_pythonIdentity(t *testing.T) {
	version := pythonOracleVersion(t)
	cases := []oracleCase{
		{
			name:   "jsonDefaultSeparatorsAndInsertionOrder",
			python: `print(json.dumps({"b": [1, 2.0, True, None], "a": "x"}))`,
			star:   `print(json.dumps({"b": [1, 2.0, True, None], "a": "x"}))`,
		},
		{
			name:   "jsonSortKeys",
			python: `print(json.dumps({"b": 1, "a": 2}, sort_keys=True))`,
			star:   `print(json.dumps({"b": 1, "a": 2}, sort_keys=True))`,
		},
		{
			name:   "jsonIntegerIndent",
			python: `print(json.dumps({"a": [1, {"b": 2}], "empty": []}, indent=2))`,
			star:   `print(json.dumps({"a": [1, {"b": 2}], "empty": []}, indent=2))`,
		},
		{
			name:   "jsonStringIndent",
			python: `print(json.dumps({"a": [1, {"b": 2}]}, indent="\t"))`,
			star:   `print(json.dumps({"a": [1, {"b": 2}]}, indent="\t"))`,
		},
		{
			name:   "jsonFloatFormatting",
			python: `print(json.dumps([1.0, 1e15, 1e16, 1e20, 1e21, 1e-6, 1e-7, -0.0, 1.2345678901234567, 5e-324, 1.7976931348623157e308]))`,
			star:   `print(json.dumps([1.0, 1e15, 1e16, 1e20, 1e21, 1e-6, 1e-7, -0.0, 1.2345678901234567, 5e-324, 1.7976931348623157e308]))`,
		},
		{
			name:   "jsonNonFiniteFloats",
			python: `print(json.dumps([float("nan"), float("inf"), float("-inf")]))`,
			star:   `print(json.dumps([float("nan"), float("inf"), float("-inf")]))`,
		},
		{
			name:   "jsonUnicodeEscapes",
			python: `print(json.dumps({"text": "A\né😀\u2028"}))`,
			star:   `print(json.dumps({"text": "A\né😀\u2028"}))`,
		},
		{
			name:   "jsonLoadsRoundTrip",
			python: `print(json.dumps(json.loads("{\"a\":[1,2.5,null]}"), sort_keys=True))`,
			star:   `print(json.dumps(json.loads("{\"a\":[1,2.5,null]}"), sort_keys=True))`,
		},
		{
			name: "regexpMatchObject",
			python: `m = re.search(r"(?P<letter>a)?(b)", "xb")
print(m.group(0))
print(m.group(1))
print(m.group(2))
print(m.group("letter"))
print(json.dumps(m.groups()))
print(json.dumps(m.groups("Z")))
print(m.start(), m.end(), m.span())
print(m.start(1), m.end(1), m.span(1))
print(json.dumps(m.group(0, 2)))`,
			star: `m = re.search(r"(?P<letter>a)?(b)", "xb")
print(m.group(0))
print(m.group(1))
print(m.group(2))
print(m.group("letter"))
print(json.dumps(m.groups()))
print(json.dumps(m.groups("Z")))
print(m.start(), m.end(), m.span())
print(m.start(1), m.end(1), m.span(1))
print(json.dumps(m.group(0, 2)))`,
		},
		{
			name: "regexpMatchModes",
			python: `print(re.match("b", "xb"))
print(re.fullmatch("a|ab", "ab"))
print(re.search("b", "xb").group())
print(re.fullmatch("ab", "ab").span())`,
			star: `print(re.match("b", "xb"))
print(re.fullmatch("a|ab", "ab"))
print(re.search("b", "xb").group())
print(re.fullmatch("ab", "ab").span())`,
		},
		{
			name: "regexpFindallGroupShapes",
			python: `print(json.dumps(re.findall(r"(a)?b", "ab b")))
print(json.dumps(re.findall(r"(a)(b)?", "ab a")))
print(json.dumps(re.findall(r"[a-z]+", "ab 12 cd")))`,
			star: `print(json.dumps(re.findall(r"(a)?b", "ab b")))
print(json.dumps(re.findall(r"(a)(b)?", "ab a")))
print(json.dumps(re.findall(r"[a-z]+", "ab 12 cd")))`,
		},
		{
			name: "regexpSubstitution",
			python: `print(re.sub(r"(a)?b", r"[\1]", "ab b"))
print(re.sub("a", "X", "a a", 1))
print(re.sub(r"(?P<letter>a)", r"<\g<letter>>", "aba"))
print(re.sub("a", r"\\", "a"))
print(re.sub("a", r"\377", "a"))`,
			star: `print(re.sub(r"(a)?b", r"[\1]", "ab b"))
print(re.sub("a", "X", "a a", 1))
print(re.sub(r"(?P<letter>a)", r"<\g<letter>>", "aba"))
print(re.sub("a", r"\\", "a"))
print(re.sub("a", r"\377", "a"))`,
		},
		{
			name: "regexpSplit",
			python: `print(json.dumps(re.split(r"(a)?(b)", "b")))
print(json.dumps(re.split(",", "a,b,c", 1)))
print(json.dumps(re.split("x*", "abc")))`,
			star: `print(json.dumps(re.split(r"(a)?(b)", "b")))
print(json.dumps(re.split(",", "a,b,c", 1)))
print(json.dumps(re.split("x*", "abc")))`,
		},
		{
			name: "regexpEmptyMatches",
			python: `print(json.dumps(re.findall("x*", "abc")))
print(re.sub("x*", "X", "abc"))
print(json.dumps(re.findall("a*", "aa")))
print(re.sub("a*", "X", "aa"))
print(json.dumps(re.findall("a*", "aab")))
print(re.sub("a*", "-", "baac"))
print(re.sub("a*", "-", "baac", 2))
print(re.sub("a|x*", "X", "ab", 2))
print(json.dumps(re.split("a*", "baac", 2)))
print(json.dumps(re.findall(r"a*|\^", "aa")))
print(json.dumps(re.findall(r"a|^", "a")))
print(json.dumps(re.findall(r"a|\b", "a")))
print(json.dumps(re.findall(r"a|\B", "a")))`,
			star: `print(json.dumps(re.findall("x*", "abc")))
print(re.sub("x*", "X", "abc"))
print(json.dumps(re.findall("a*", "aa")))
print(re.sub("a*", "X", "aa"))
print(json.dumps(re.findall("a*", "aab")))
print(re.sub("a*", "-", "baac"))
print(re.sub("a*", "-", "baac", 2))
print(re.sub("a|x*", "X", "ab", 2))
print(json.dumps(re.split("a*", "baac", 2)))
print(json.dumps(re.findall(r"a*|\^", "aa")))
print(json.dumps(re.findall(r"a|^", "a")))
print(json.dumps(re.findall(r"a|\b", "a")))
print(json.dumps(re.findall(r"a|\B", "a")))`,
		},
		{
			name: "regexpCompiledPattern",
			python: `p = re.compile(r"(?P<a>a)(b)?")
m = p.fullmatch("a")
print(m.group())
print(json.dumps(m.groups()))
print(m.span(2))
print(json.dumps(p.findall("ab a")))
print(p.sub(r"<\g<a>>", "ab a", 1))
print(json.dumps(p.split("ab a")))
print(p.search("zzab").span())`,
			star: `p = re.compile(r"(?P<a>a)(b)?")
m = p.fullmatch("a")
print(m.group())
print(json.dumps(m.groups()))
print(m.span(2))
print(json.dumps(p.findall("ab a")))
print(p.sub(r"<\g<a>>", "ab a", 1))
print(json.dumps(p.split("ab a")))
print(p.search("zzab").span())`,
		},
		{
			name: "regexpUnicodeOffsets",
			python: `m = re.search("(é)(😀)?", "xé")
print(m.start(), m.end(), m.span())
print(m.start(2), m.end(2), m.span(2))
print(m)`,
			star: `m = re.search("(é)(😀)?", "xé")
print(m.start(), m.end(), m.span())
print(m.start(2), m.end(2), m.span(2))
print(m)`,
		},
		{
			name:   "mathModule",
			python: `print(math.sqrt(9), math.floor(-1.2), math.ceil(1.2), math.pi)`,
			star:   `print(math.sqrt(9), math.floor(-1.2), math.ceil(1.2), math.pi)`,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			want := runPython(t, version, tc.python)
			got := runStarlark(t, tc.star, stdlib.Hooks{Seed: 7})
			if got != want {
				t.Fatalf("output mismatch\n--- Starlark ---\n%s--- CPython %s ---\n%s", got, version, want)
			}
		})
	}
}

func TestPredeclared_timeUsesInjectedEffects(t *testing.T) {
	t.Parallel()
	var slept []time.Duration
	hooks := stdlib.Hooks{
		Now: func() float64 { return 12.5 },
		Sleep: func(d time.Duration) error {
			slept = append(slept, d)
			return nil
		},
	}
	got := runStarlark(t, "print(time.time())\ntime.sleep(1.25)\nprint(\"done\")", hooks)
	if got != "12.5\ndone\n" {
		t.Fatalf("output = %q", got)
	}
	if len(slept) != 1 || slept[0] != 1250*time.Millisecond {
		t.Fatalf("sleep effects = %v, want [1.25s]", slept)
	}
}

func TestPredeclared_timePropagatesSleepFailure(t *testing.T) {
	t.Parallel()
	want := errors.New("replay interrupted")
	_, err := execStarlark("time.sleep(2)", stdlib.Hooks{
		Sleep: func(time.Duration) error { return want },
	})
	if !errors.Is(err, want) {
		t.Fatalf("error = %v, want wrapped %v", err, want)
	}
}

func TestPredeclared_randomIsSeededAndPythonShaped(t *testing.T) {
	t.Parallel()
	const source = `xs = [1, 2, 3, 4, 5]
random.shuffle(xs)
floats = [random.random() for _ in range(8)]
ints = [random.randint(-3, 4) for _ in range(16)]
uniform = random.uniform(-2, 3)
tuple_choice = random.choice((4, 5, 6))
string_choice = random.choice("abc")
print(json.dumps(xs))
print(json.dumps(floats))
print(json.dumps(ints))
print(uniform)
print(tuple_choice)
print(string_choice)`
	first := runStarlark(t, source, stdlib.Hooks{Seed: 99})
	second := runStarlark(t, source, stdlib.Hooks{Seed: 99})
	if first != second {
		t.Fatalf("same seed produced different streams\n%s\n%s", first, second)
	}
	globals, err := execStarlark(source, stdlib.Hooks{Seed: 99})
	if err != nil {
		t.Fatalf("inspect random results: %v", err)
	}
	assertShuffledIntegers(t, globals["xs"])
	assertFloatRange(t, globals["floats"], 0, 1)
	assertIntRange(t, globals["ints"], -3, 4)
	assertFloatRange(t, globals["uniform"], -2, 3)
	assertRandomChoices(t, globals)
}

func assertRandomChoices(t *testing.T, globals starlark.StringDict) {
	t.Helper()
	choice, ok := globals["tuple_choice"].(starlark.Int)
	if !ok {
		t.Fatalf("tuple choice type = %T", globals["tuple_choice"])
	}
	value, ok := choice.Int64()
	if !ok || value < 4 || value > 6 {
		t.Fatalf("tuple choice = %v, want one of 4, 5, 6", choice)
	}
	char, ok := starlark.AsString(globals["string_choice"])
	if !ok || len(char) != 1 || char < "a" || char > "c" {
		t.Fatalf("string choice = %v, want one ASCII character from abc", globals["string_choice"])
	}
}

func assertShuffledIntegers(t *testing.T, value starlark.Value) {
	t.Helper()
	list, ok := value.(*starlark.List)
	if !ok || list.Len() != 5 {
		t.Fatalf("shuffled value = %v, want a five-item list", value)
	}
	seen := map[int64]bool{}
	for i := range list.Len() {
		item, ok := list.Index(i).(starlark.Int)
		if !ok {
			t.Fatalf("shuffled item %d has type %T", i, list.Index(i))
		}
		n, ok := item.Int64()
		if !ok {
			t.Fatalf("shuffled item %d is outside int64: %v", i, item)
		}
		seen[n] = true
	}
	for n := int64(1); n <= 5; n++ {
		if !seen[n] {
			t.Errorf("shuffle lost integer %d: %v", n, value)
		}
	}
}

func assertFloatRange(t *testing.T, value starlark.Value, low, high float64) {
	t.Helper()
	values, ok := value.(*starlark.List)
	if !ok {
		f, ok := starlark.AsFloat(value)
		if !ok || f < low || f > high {
			t.Fatalf("float = %v, want range [%v, %v]", value, low, high)
		}
		return
	}
	for i := range values.Len() {
		f, ok := starlark.AsFloat(values.Index(i))
		if !ok || f < low || f >= high {
			t.Fatalf("float item %d = %v, want range [%v, %v)", i, values.Index(i), low, high)
		}
	}
}

func assertIntRange(t *testing.T, value starlark.Value, low, high int64) {
	t.Helper()
	list, ok := value.(*starlark.List)
	if !ok {
		t.Fatalf("integers = %v, want list", value)
	}
	for i := range list.Len() {
		item, ok := list.Index(i).(starlark.Int)
		if !ok {
			t.Fatalf("integer item %d has type %T", i, list.Index(i))
		}
		n, ok := item.Int64()
		if !ok || n < low || n > high {
			t.Fatalf("integer item %d = %v, want range [%d, %d]", i, item, low, high)
		}
	}
}

func TestPredeclared_rejectsPythonOnlyRegexpSyntax(t *testing.T) {
	t.Parallel()
	_, err := execStarlark(`re.compile(r"(a)\1")`, stdlib.Hooks{})
	if err == nil || !strings.Contains(err.Error(), "re: unsupported syntax:") {
		t.Fatalf("error = %v, want unsupported-syntax diagnostic", err)
	}
}

func TestPredeclared_rejectsNegativeSleep(t *testing.T) {
	t.Parallel()
	called := false
	_, err := execStarlark("time.sleep(-0.01)", stdlib.Hooks{
		Sleep: func(time.Duration) error {
			called = true
			return nil
		},
	})
	if err == nil || called {
		t.Fatalf("error = %v, sleep hook called = %v", err, called)
	}
}

func TestFileOptions_enablesRequiredDialect(t *testing.T) {
	t.Parallel()
	opts := stdlib.FileOptions()
	if !opts.Set || !opts.While || !opts.TopLevelControl || !opts.GlobalReassign || !opts.Recursion {
		t.Fatalf("dialect options = %+v", opts)
	}
}

func TestPredeclared_rejectsCyclesAndInvalidGroups(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		source string
		want   string
	}{
		{name: "jsonCycle", source: "x = []\nx.append(x)\njson.dumps(x)", want: "cycle in JSON structure"},
		{name: "missingGroup", source: `re.search("(a)", "a").group(2)`, want: "no such group"},
		{name: "missingReplacementGroup", source: `re.sub("(a)", r"\2", "a")`, want: "invalid group reference"},
		{name: "emptyChoice", source: "random.choice([])", want: "empty sequence"},
		{name: "invalidIndent", source: `json.dumps({}, indent=True)`, want: "want int, string, or None"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := execStarlark(tc.source, stdlib.Hooks{})
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want substring %q", err, tc.want)
			}
		})
	}
}

func runStarlark(t *testing.T, source string, hooks stdlib.Hooks) string {
	t.Helper()
	var out bytes.Buffer
	thread := &starlark.Thread{
		Name: "oracle",
		Print: func(_ *starlark.Thread, msg string) {
			out.WriteString(msg)
			out.WriteByte('\n')
		},
	}
	opts := stdlib.FileOptions()
	_, err := starlark.ExecFileOptions(&opts, thread, "oracle.star", source, stdlib.Predeclared(hooks))
	if err != nil {
		t.Fatalf("execute Starlark: %v\nsource:\n%s", err, source)
	}
	return out.String()
}

func execStarlark(source string, hooks stdlib.Hooks) (starlark.StringDict, error) {
	opts := stdlib.FileOptions()
	thread := &starlark.Thread{Name: "test"}
	return starlark.ExecFileOptions(&opts, thread, "test.star", source, stdlib.Predeclared(hooks))
}

func pythonOracleVersion(t *testing.T) string {
	t.Helper()
	if _, err := exec.LookPath("uv"); err != nil {
		t.Skip("uv is not installed")
	}
	for _, version := range []string{"3.15", "3.14", "3.13"} {
		cmd := exec.Command("uv", "run", "--no-project", "--python", version, "python", "-c", "import sys; assert sys.version_info >= (3, 13)")
		if err := cmd.Run(); err == nil {
			return version
		}
	}
	t.Skip("uv could not provide CPython 3.13 or newer")
	return ""
}

func runPython(t *testing.T, version, source string) string {
	t.Helper()
	imports := "import json, math, random, re\n"
	cmd := exec.Command("uv", "run", "--no-project", "--python", version, "python", "-c", imports+source)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		t.Fatalf("execute CPython %s: %v\nstderr:\n%s\nsource:\n%s", version, err, stderr.String(), source)
	}
	return strings.ReplaceAll(stdout.String(), "\r\n", "\n")
}
