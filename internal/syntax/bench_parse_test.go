package syntax

import (
	"bytes"
	"context"
	"fmt"
	"math/rand/v2"
	"os"
	"os/exec"
	"regexp"
	"slices"
	"strconv"
	"testing"
	"time"
)

var benchLanguages = []string{
	"bash", "c", "c_sharp", "cpp", "go", "java", "javascript", "kotlin", "lua",
	"markdown", "php", "python", "ruby", "rust", "swift", "tsx", "typescript",
}

const firstUseTag = "FIRSTUSE_NS "

type benchTemplate struct {
	header string
	defs   []string
}

var benchTemplates = map[string]benchTemplate{
	"bash": {defs: []string{
		`{{a}}() {
  echo start
}`,
		`function {{a}} {
  echo done
}`,
		`{{a}}() {
  return 0
}`,
		`{{a}}() {
  local x=1
  echo "$x"
}`,
		`{{a}}() { echo one; }`,
		`{{a}}() {
  echo two
}`,
		`function {{a}} () {
  echo three
}`,
		`{{a}}() {
  echo four
}`,
		`{{a}}() {
  echo five
}`,
		`function {{a}} {
  echo six
}`,
	}},
	"c": {defs: []string{
		`int {{a}}(int x) {
    return x;
}`,
		`struct {{A}} {
    int w;
};`,
		`union {{A}} {
    int w;
    float f;
};`,
		`typedef int {{A}};`,
		`enum {{A}} {
    {{A}}_ONE,
    {{A}}_TWO
};`,
		`static int {{a}} = 2;`,
		`const char *{{a}}(void) {
    return 0;
}`,
		`void {{a}}(void) {
}`,
		`double {{a}}(double x) {
    return x * 2.0;
}`,
		`int {{a}}(void) {
    return 1;
}`,
	}},
	"c_sharp": {defs: []string{
		`public class {{A}} {
    public {{A}}() {}
    public int {{a}}() { return 1; }
}`,
		`public interface I{{A}} {
    void {{a}}();
}`,
		`public enum {{A}} { One, Two }`,
		`public struct {{A}} {
    public int X;
}`,
		`public record {{A}}(int X);`,
		`public static class {{A}}Tools {
    public static int {{a}}() { return 2; }
}`,
		`public class {{A}} : Base {
    public void {{a}}() {}
}`,
		`internal class {{A}} {}`,
		`public interface {{A}} { }`,
		`public class {{A}}Box<T> {
    public T {{a}}(T x) { return x; }
}`,
	}},
	"cpp": {defs: []string{
		`int {{a}}(int x) {
    return x;
}`,
		`class {{A}} {
public:
    void {{a}}() {}
};`,
		`struct {{A}} {
    int w;
};`,
		`typedef int {{A}};`,
		`namespace {{a}} {
}`,
		`enum class {{A}} { One, Two };`,
		`void {{A}}::{{a}}() {}`,
		`template <typename T>
T {{a}}(T x) {
    return x;
}`,
		`union {{A}} {
    int w;
};`,
		`static int {{a}} = 2;`,
	}},
	"go": {header: "package p\n\n", defs: []string{
		`func {{a}}(x int) int {
  return x
}`,
		`type {{A}} struct {
  W int
}`,
		`type {{A}} interface {
  {{a}}() string
}`,
		`type {{A}} = int`,
		`const {{a}} = 8`,
		`var {{a}} = 2`,
		`func (s {{A}}) {{a}}() int {
  return 1
}`,
		`func {{a}}() {}`,
		`type {{A}} int`,
		`var {{a}} int`,
	}},
	"java": {defs: []string{
		`public class {{A}} {
    public {{A}}() {}
    public int {{a}}() { return 1; }
}`,
		`interface {{A}} {
    void {{a}}();
}`,
		`enum {{A}} { ONE, TWO }`,
		`record {{A}}(int x, int y) {}`,
		`@interface {{A}} {}`,
		`class {{A}} extends Base {
    void {{a}}() {}
}`,
		`final class {{A}} {
    static int {{a}}(int x) { return x; }
}`,
		`abstract class {{A}} {
    abstract void {{a}}();
}`,
		`class {{A}}<T> {
    T {{a}}(T x) { return x; }
}`,
		`interface {{A}} extends Runnable {
}`,
	}},
	"javascript": {defs: []string{
		`function {{a}}(a, b) {
  return a + b;
}`,
		`class {{A}} {
  {{a}}() {
    return 1;
  }
}`,
		`const {{a}} = (n) => n * 2;`,
		`function* {{a}}() {
  yield 1;
}`,
		`class {{A}} extends Base {
  #hide() {}
}`,
		`let {{a}} = 3;`,
		`var {{a}} = function () {
  return 4;
};`,
		`async function {{a}}() {
  return 5;
}`,
		`export function {{a}}() {}`,
		`const {{A}} = { {{a}}: 1 };`,
	}},
	"kotlin": {defs: []string{
		`class {{A}} {
    fun {{a}}(): Int = 1
}`,
		`interface {{A}}`,
		`enum class {{A}} { ONE, TWO }`,
		`object {{A}}`,
		`typealias {{A}} = Int`,
		`fun {{a}}(x: Int): Int {
    return x
}`,
		`data class {{A}}(val x: Int)`,
		`val {{a}} = 2`,
		`sealed class {{A}}`,
		`abstract class {{A}} {
    abstract fun {{a}}()
}`,
	}},
	"lua": {defs: []string{
		`local function {{a}}()
  return 1
end`,
		`function {{A}}.{{a}}(self)
  return self
end`,
		`function {{A}}:{{a}}()
  return self
end`,
		`function {{a}}()
end`,
		`local {{a}} = 2`,
		`{{A}} = {}`,
		`function {{a}}(x)
  return x * 2
end`,
		`local function {{a}}(a, b)
  return a + b
end`,
		`{{a}} = function()
  return 3
end`,
		`function {{A}}.{{a}}()
end`,
	}},
	"markdown": {defs: []string{
		`# {{A}}

Some text.`,
		`## {{a}}

More text.`,
		`### {{a}}

Deeper text.`,
		`#### {{a}}

Text here.`,
		`Paragraph with {{a}} in it.`,
		`- item one
- item two`,
		`> quoted {{a}}`,
		`1. first
2. second`,
		`Text with code.`,
		`[link](https://example.com)`,
	}},
	"php": {header: "<?php\n\n", defs: []string{
		`function {{a}}($x) {
    return $x;
}`,
		`class {{A}} {
    public function {{a}}() {}
}`,
		`interface {{A}} {}`,
		`trait {{A}} {}`,
		`enum {{A}} {
    case {{A}};
}`,
		`const {{A}} = 8;`,
		`abstract class {{A}} {
    abstract public function {{a}}();
}`,
		`final class {{A}} extends Base {}`,
		`function {{a}}(int $x): int {
    return $x * 2;
}`,
		`${{a}} = 3;`,
	}},
	"python": {defs: []string{
		`def {{a}}(x):
    return x`,
		`class {{A}}:
    def {{a}}(self):
        return 1`,
		`@cache
def {{a}}(url):
    return url`,
		`class {{A}}(Base):
    pass`,
		`{{a}} = 2`,
		`async def {{a}}():
    return 3`,
		`class {{A}}:
    @property
    def {{a}}(self):
        return 4`,
		`def {{a}}(*args, **kwargs):
    return args`,
		`{{A}} = {{a}}`,
		`def {{a}}() -> int:
    return 5`,
	}},
	"ruby": {defs: []string{
		`def {{a}}(x)
  x
end`,
		`class {{A}}
  def {{a}}
    1
  end
end`,
		`module {{A}}
  {{A}} = 8
end`,
		`{{A}} = 2`,
		`def {{a}}
  2
end`,
		`class {{A}} < Base
end`,
		`def {{a}}?
  true
end`,
		`class {{A}}
  attr_reader :{{a}}
end`,
		`module {{A}}::{{A}}
end`,
		`def {{a}}(*args)
  args
end`,
	}},
	"rust": {defs: []string{
		`pub fn {{a}}(x: i32) -> i32 {
    x
}`,
		`pub struct {{A}} {
    pub w: i32,
}`,
		`pub enum {{A}} {
    One,
}`,
		`pub trait {{A}} {
    fn {{a}}(&self) -> i32;
}`,
		`impl {{A}} {
    pub fn {{a}}() -> i32 {
        1
    }
}`,
		`pub const {{A}}: i32 = 8;`,
		`pub static {{a}}: i32 = 2;`,
		`pub type {{A}} = i32;`,
		`pub mod {{a}} {
}`,
		`fn {{a}}() {}`,
	}},
	"swift": {defs: []string{
		`protocol {{A}} {
    func {{a}}() -> String
}`,
		`class {{A}} {
    func {{a}}() {}
}`,
		`struct {{A}} {
    var x: Int
}`,
		`enum {{A}} {
    case {{a}}
}`,
		`typealias {{A}} = Int`,
		`func {{a}}(x: Int) -> Int {
    return x
}`,
		`actor {{A}} {}`,
		`let {{a}} = 2`,
		`extension {{A}} {
    func {{a}}() {}
}`,
		`class {{A}}: Base {}`,
	}},
	"tsx": {defs: []string{
		`export function {{a}}(props: { title: string }) {
  return <div>{props.title}</div>;
}`,
		`export const {{A}} = () => <button>ok</button>;`,
		`export interface {{A}} {
  title: string;
}`,
		`export function {{a}}(): JSX.Element {
  return <span />;
}`,
		`const {{a}} = (n: number) => <p>{n}</p>;`,
		`type {{A}} = { x: number };`,
		`export class {{A}} {
  open(): void {}
}`,
		`enum {{A}} { One, Two }`,
		`export const {{a}}: number = 3;`,
		`export function {{a}}() {
  return <i />;
}`,
	}},
	"typescript": {defs: []string{
		`export function {{a}}(name: string): string {
  return name;
}`,
		`export class {{A}} {
  open(): void {}
  private seal(): void {}
}`,
		`export abstract class {{A}} {
  abstract run(): void;
}`,
		`export interface {{A}} {
  area(): number;
}`,
		`export enum {{A}} {
  Red,
}`,
		`export type {{A}} = [number, number];`,
		`export const {{a}} = (n: number) => n * 2;`,
		`export namespace {{A}} {
  export const v = 1;
}`,
		`export function {{a}}(): void {}`,
		`export let {{a}}: number = 1;`,
	}},
}

var benchPlaceholder = regexp.MustCompile(`\{\{[aA]\}\}`)

const benchLetters = "abcdefghijklmnopqrstuvwxyz"

func benchIdent(r *rand.Rand, first byte) string {
	name := []byte("nxxxxxxx")
	name[0] = first
	for i := 1; i < len(name); i++ {
		name[i] = benchLetters[r.IntN(len(benchLetters))]
	}
	return string(name)
}

func benchLowerName(r *rand.Rand) string {
	return benchIdent(r, 'n')
}

func benchUpperName(r *rand.Rand) string {
	return benchIdent(r, 'N')
}

func benchSource(tb testing.TB, name string, size int) []byte {
	tb.Helper()
	tpl, ok := benchTemplates[name]
	if !ok {
		tb.Fatalf("no benchmark template for %s", name)
	}
	names := rand.New(rand.NewPCG(1, 2))
	out := []byte(tpl.header)
	for i := 0; ; i++ {
		def := benchPlaceholder.ReplaceAllStringFunc(tpl.defs[i%len(tpl.defs)], func(token string) string {
			if token == "{{A}}" {
				return benchUpperName(names)
			}
			return benchLowerName(names)
		})
		if len(out)+len(def)+2 > size {
			break
		}
		out = append(out, def...)
		out = append(out, '\n', '\n')
	}
	if len(out) == len(tpl.header) {
		tb.Fatalf("source for %s is smaller than one definition at %d bytes", name, size)
	}
	return out
}

func reportSamples(b *testing.B, samples []time.Duration) {
	b.Helper()
	slices.Sort(samples)
	at := func(p float64) float64 {
		return float64(samples[min(len(samples)-1, int(p*float64(len(samples))))].Nanoseconds())
	}
	b.ReportMetric(at(0.50), "p50_ns")
	b.ReportMetric(at(0.95), "p95_ns")
	b.ReportMetric(at(0.99), "p99_ns")
	b.ReportMetric(float64(samples[len(samples)-1].Nanoseconds()), "max_ns")
}

func BenchmarkSymbolsThroughput(b *testing.B) {
	for _, name := range benchLanguages {
		for _, size := range []int{4 << 10, 64 << 10, 1 << 20} {
			b.Run(fmt.Sprintf("%s/%dKB", name, size>>10), func(b *testing.B) {
				benchSymbols(b, name, size)
			})
		}
	}
}

func benchSymbols(b *testing.B, name string, size int) {
	b.Helper()
	lang, ok := ByName(name)
	if !ok {
		b.Fatalf("unknown language %s", name)
	}
	src := benchSource(b, name, size)
	parser := lang.NewParser()
	ctx := context.Background()
	_, report, err := parser.Symbols(ctx, src)
	if err != nil {
		b.Fatal(err)
	}
	if report.SyntaxErrors {
		b.Fatalf("%s at %d KB: the template parses with syntax errors", name, size>>10)
	}
	b.SetBytes(int64(len(src)))
	b.ReportAllocs()
	samples := make([]time.Duration, 0, b.N)
	stopped, syntaxErrors := 0, 0
	b.ResetTimer()
	for range b.N {
		start := time.Now()
		_, report, err := parser.Symbols(ctx, src)
		samples = append(samples, time.Since(start))
		if err != nil {
			b.Fatal(err)
		}
		if report.Stopped {
			stopped++
		}
		if report.SyntaxErrors {
			syntaxErrors++
		}
	}
	reportSamples(b, samples)
	b.ReportMetric(float64(stopped), "stopped")
	b.ReportMetric(float64(syntaxErrors), "syntax_errors")
}

func BenchmarkFirstUse(b *testing.B) {
	for _, name := range []string{"go", "python", "cpp"} {
		b.Run(name, func(b *testing.B) {
			samples := make([]time.Duration, 0, 30)
			for range 30 {
				samples = append(samples, benchFirstUseSample(b, name))
			}
			reportSamples(b, samples)
		})
	}
}

func benchFirstUseSample(b *testing.B, name string) time.Duration {
	b.Helper()
	cmd := exec.Command(os.Args[0], "-test.run=^TestFirstUseProbe$", "-test.count=1") //nolint:gosec // re-executes this test binary with the probe test only
	cmd.Env = append(os.Environ(), "DALGONA_SYNTAX_PROBE="+name)
	out, err := cmd.CombinedOutput()
	if err != nil {
		b.Fatalf("probe %s: %v\n%s", name, err, out)
	}
	_, rest, found := bytes.Cut(out, []byte(firstUseTag))
	if !found {
		b.Fatalf("probe %s printed no timing:\n%s", name, out)
	}
	line, _, _ := bytes.Cut(rest, []byte("\n"))
	ns, err := strconv.ParseInt(string(line), 10, 64)
	if err != nil {
		b.Fatal(err)
	}
	return time.Duration(ns)
}

func TestFirstUseProbe(t *testing.T) {
	name := os.Getenv("DALGONA_SYNTAX_PROBE")
	if name == "" {
		t.Skip("probe runs only as the child of BenchmarkFirstUse")
	}
	lang, ok := ByName(name)
	if !ok {
		t.Fatalf("unknown language %s", name)
	}
	src := benchSource(t, name, 4<<10)
	ctx := context.Background()
	start := time.Now()
	if _, _, err := lang.NewParser().Symbols(ctx, src); err != nil {
		t.Fatal(err)
	}
	if _, err := fmt.Fprintf(os.Stdout, "%s%d\n", firstUseTag, time.Since(start).Nanoseconds()); err != nil {
		t.Fatal(err)
	}
}
