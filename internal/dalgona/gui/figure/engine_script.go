package figure

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"embed"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"path"
	"strings"
	"sync"
	"unicode/utf8"

	"modernc.org/quickjs"
	"oss.terrastruct.com/d2/d2renderers/d2fonts"
)

//go:embed js
var engineFiles embed.FS

//go:embed js/SHA256SUMS
var engineSums []byte

var EngineManifest = sha256.Sum256(engineSums)

var verifyEngines = sync.OnceValue(func() error {
	sc := bufio.NewScanner(bytes.NewReader(engineSums))
	listed := 0
	for sc.Scan() {
		sum, name, ok := strings.Cut(sc.Text(), "  ")
		if !ok {
			return fail(FailureEngine, "engine manifest line %q is malformed", sc.Text())
		}
		data, err := engineFiles.ReadFile("js/" + name)
		if err != nil {
			return fail(FailureEngine, "engine file %s is missing from the build", name)
		}
		if got := sha256.Sum256(data); hex.EncodeToString(got[:]) != sum {
			return fail(FailureEngine, "engine file %s does not match its pinned SHA-256", name)
		}
		listed++
	}
	if listed == 0 {
		return fail(FailureEngine, "engine manifest lists no files")
	}
	return nil
})

const scriptPrelude = `
globalThis.global = globalThis;
globalThis.console = { log: function () {}, warn: function () {}, error: function () {}, info: function () {}, debug: function () {} };
globalThis.setTimeout = function (fn, delay) {
	if (delay === 0 || delay === undefined) { fn(); return 0; }
	throw new Error("timers are unavailable to figure engines");
};
globalThis.atob = function (input) {
	var alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	var out = "", bits = 0, count = 0;
	for (var i = 0; i < input.length; i++) {
		var v = alphabet.indexOf(input.charAt(i));
		if (v < 0) { continue; }
		bits = (bits << 6) | v;
		count += 6;
		if (count >= 8) { count -= 8; out += String.fromCharCode((bits >> count) & 255); }
	}
	return out;
};
`

var scriptModules = map[string]string{
	"beautiful-mermaid":        "beautiful-mermaid/index.js",
	"elkjs/lib/elk.bundled.js": "elkjs/elk.bundled.js",
	"entities":                 "entities/index.js",
}

const commonJSModules = "elkjs/elk.bundled.js"

func normalizeModule(_ *quickjs.VM, base, name string) (string, error) {
	if mapped, ok := scriptModules[name]; ok {
		return mapped, nil
	}
	if strings.HasPrefix(name, ".") {
		return path.Join(path.Dir(base), name), nil
	}
	return "", fmt.Errorf("module %s is not a vendored figure engine module", name)
}

func loadModule(_ *quickjs.VM, name string) (string, error) {
	if name != path.Clean(name) || strings.HasPrefix(name, "..") || !strings.HasSuffix(name, ".js") {
		return "", fmt.Errorf("module %s is not a vendored figure engine module", name)
	}
	data, err := engineFiles.ReadFile("js/" + name)
	if err != nil {
		return "", fmt.Errorf("module %s is not a vendored figure engine module", name)
	}
	if name == commonJSModules {
		return "const module = { exports: {} }; const exports = module.exports;\n" + string(data) + "\nexport default module.exports;", nil
	}
	return string(data), nil
}

func newScriptVM() (*quickjs.VM, error) {
	if err := verifyEngines(); err != nil {
		return nil, err
	}
	vm, err := quickjs.NewVM()
	if err != nil {
		return nil, fail(FailureEngine, "figure engine runtime failed to start: %v", err)
	}
	vm.SetMemoryLimit(QuickJSHeapLimit)
	vm.SetMaxStackSize(QuickJSStackSlots)
	if _, err := vm.Eval(scriptPrelude, quickjs.EvalGlobal); err != nil {
		return nil, closeOnFailure(vm, fail(FailureEngine, "figure engine prelude failed: %v", err))
	}
	return vm, nil
}

func closeOnFailure(vm *quickjs.VM, failure *Failure) *Failure {
	if err := vm.Close(); err != nil {
		failure.Message += fmt.Sprintf("; closing the engine runtime also failed: %v", err)
	}
	return failure
}

const mermaidEntry = `
import { renderMermaidSVG, parseMermaid } from "beautiful-mermaid";
globalThis.__render = function (text, options) {
	return renderMermaidSVG(text, JSON.parse(options));
};
globalThis.__graph = function (text) {
	var g;
	try { g = parseMermaid(text); } catch (e) { return "0 0"; }
	return (g.nodes ? g.nodes.size : 0) + " " + (g.edges ? g.edges.length : 0);
};
`

func newMermaidVM() (*quickjs.VM, error) {
	vm, err := newScriptVM()
	if err != nil {
		return nil, err
	}
	vm.SetModuleLoader(loadModule, normalizeModule)
	if _, err := vm.Eval(mermaidEntry, quickjs.EvalModule); err != nil {
		return nil, closeOnFailure(vm, fail(FailureEngine, "Mermaid engine failed to load: %v", err))
	}
	if _, err := vm.ExecutePendingJobs(); err != nil {
		return nil, closeOnFailure(vm, fail(FailureEngine, "Mermaid engine failed to load: %v", err))
	}
	return vm, nil
}

func newNomnomlVM() (*quickjs.VM, error) {
	vm, err := newScriptVM()
	if err != nil {
		return nil, err
	}
	graphre, err := engineFiles.ReadFile("js/graphre/graphre.js")
	if err != nil {
		return nil, closeOnFailure(vm, fail(FailureEngine, "nomnoml engine is missing graphre"))
	}
	nomnoml, err := engineFiles.ReadFile("js/nomnoml/nomnoml.js")
	if err != nil {
		return nil, closeOnFailure(vm, fail(FailureEngine, "nomnoml engine is missing nomnoml"))
	}
	var src strings.Builder
	src.WriteString("var module = { exports: {} }; var exports = module.exports; var __deps = {};\nfunction require(name) { if (!(name in __deps)) { throw new Error('module ' + name + ' is not vendored'); } return __deps[name]; }\n")
	src.Write(graphre)
	src.WriteString("\n__deps.graphre = module.exports; module = { exports: {} }; exports = module.exports;\n")
	src.Write(nomnoml)
	src.WriteString(nomnomlEntry)
	if _, err := vm.Eval(src.String(), quickjs.EvalGlobal); err != nil {
		return nil, closeOnFailure(vm, fail(FailureEngine, "nomnoml engine failed to load: %v", err))
	}
	return vm, nil
}

const nomnomlEntry = `
var __nomnoml = module.exports;
globalThis.__graph = function (text) {
	var nodes = 0, edges = 0;
	function walk(part) {
		edges += part.assocs.length;
		for (var i = 0; i < part.nodes.length; i++) {
			nodes++;
			for (var j = 0; j < part.nodes[i].parts.length; j++) { walk(part.nodes[i].parts[j]); }
		}
	}
	walk(__nomnoml.parse(text).root);
	return nodes + " " + edges;
};
globalThis.__render = function (text, table) {
	var advances = JSON.parse(table);
	var context = {
		font: "",
		measureText: function (s) {
			var size = /([0-9.]+)(pt|px)/.exec(this.font);
			var px = size ? parseFloat(size[1]) * (size[2] === "pt" ? 4 / 3 : 1) : 16;
			var bold = /bold|[6-9]00/.test(this.font) ? 1 : 0;
			var width = 0;
			for (var ch of String(s)) {
				var a = advances[ch] || advances[""];
				width += a[bold] * px / 100;
			}
			return { width: width };
		}
	};
	var measuring = { createElement: function () { return { getContext: function () { return context; } }; } };
	return __nomnoml.renderSvg(text, measuring);
};
`

func callScript(ctx context.Context, vm *quickjs.VM, function string, args ...any) (any, error) {
	stop := context.AfterFunc(ctx, vm.Interrupt)
	defer stop()
	out, err := vm.Call(function, args...)
	if err != nil {
		if ctx.Err() != nil {
			return nil, fail(FailureTimeout, "figure engine stopped at its deadline")
		}
		return nil, scriptFailure(err)
	}
	return out, nil
}

func scriptFailure(err error) error {
	var jsErr *quickjs.Error
	message := err.Error()
	if errors.As(err, &jsErr) {
		message = jsErr.Error()
	}
	message = firstLine(message)
	switch {
	case strings.Contains(message, "out of memory"), strings.Contains(message, "stack overflow"):
		return fail(FailureLimit, "figure engine exceeded its memory or call depth: %s", message)
	case strings.HasPrefix(message, "Error:"), strings.HasPrefix(message, "ParseError"):
		return fail(FailureInvalid, "the engine rejected the figure: %s", strings.TrimSpace(strings.TrimPrefix(message, "Error:")))
	}
	return fail(FailureEngine, "figure engine failed: %s", message)
}

func firstLine(s string) string {
	s, _, _ = strings.Cut(s, "\n")
	if len(s) > 512 {
		s = s[:512]
	}
	return s
}

func graphSize(ctx context.Context, vm *quickjs.VM, source string) error {
	out, err := callScript(ctx, vm, "__graph", source)
	if err != nil {
		return err
	}
	counts, ok := out.(string)
	var nodes, edges int
	if n, _ := fmt.Sscanf(counts, "%d %d", &nodes, &edges); !ok || n != 2 {
		return fail(FailureEngine, "figure engine returned a malformed graph size")
	}
	if nodes > GraphNodeLimit || edges > GraphEdgeLimit {
		return fail(FailureLimit, "figure has %d nodes and %d edges; the limit is %d nodes and %d edges", nodes, edges, GraphNodeLimit, GraphEdgeLimit)
	}
	return nil
}

func mermaidOptions(t Theme) string {
	var b []byte
	b = append(b, '{')
	for i, f := range [...]struct {
		key   string
		color Color
	}{{"bg", t.Surface}, {"fg", t.Text}, {"line", t.Strong}, {"accent", t.Accent}, {"muted", t.Dim}, {"surface", t.Tone}, {"border", t.Line}} {
		if i > 0 {
			b = append(b, ',')
		}
		b, _ = jsontext.AppendQuote(b, f.key)
		b = append(b, ':')
		b, _ = jsontext.AppendQuote(b, f.color.hex())
	}
	return string(b) + `,"font":"Source Sans Pro","transparent":false}`
}

func renderMermaid(ctx context.Context, vm *quickjs.VM, source string, theme Theme) ([]byte, error) {
	if err := graphSize(ctx, vm, source); err != nil {
		return nil, err
	}
	out, err := callScript(ctx, vm, "__render", source, mermaidOptions(theme))
	if err != nil {
		return nil, err
	}
	svg, ok := out.(string)
	if !ok {
		return nil, fail(FailureEngine, "Mermaid returned %T instead of SVG text", out)
	}
	nodes, edges := strings.Count(svg, ` data-id="`), strings.Count(svg, ` data-from="`)
	if nodes > GraphNodeLimit || edges > GraphEdgeLimit {
		return nil, fail(FailureLimit, "figure has %d nodes and %d edges; the limit is %d nodes and %d edges", nodes, edges, GraphNodeLimit, GraphEdgeLimit)
	}
	return []byte(svg), nil
}

func refuseNomnomlImports(source string) error {
	for i, line := range strings.Split(source, "\n") {
		trimmed := strings.TrimLeft(line, " \t")
		if strings.HasPrefix(trimmed, "#import") {
			column := len(line) - len(trimmed) + 1
			return failAt(FailureForbidden, Span{Line: i + 1, Column: column, EndLine: i + 1, EndColumn: column + utf8.RuneCountInString(trimmed)}, "nomnoml imports are refused; paste the imported content into the figure")
		}
	}
	return nil
}

func nomnomlTheme(t Theme) string {
	return fmt.Sprintf("#stroke: %s\n#fill: %s; %s\n#background: transparent\n", t.Text.hex(), t.Surface.hex(), t.Tone.hex())
}

const nomnomlThemeLines = 3

func renderNomnoml(ctx context.Context, vm *quickjs.VM, source string, theme Theme) ([]byte, error) {
	if err := refuseNomnomlImports(source); err != nil {
		return nil, err
	}
	themed := nomnomlTheme(theme) + source
	if err := graphSize(ctx, vm, themed); err != nil {
		return nil, shiftFailure(err)
	}
	table, err := advanceTable(themed)
	if err != nil {
		return nil, err
	}
	out, err := callScript(ctx, vm, "__render", themed, table)
	if err != nil {
		return nil, shiftFailure(err)
	}
	svg, ok := out.(string)
	if !ok {
		return nil, fail(FailureEngine, "nomnoml returned %T instead of SVG text", out)
	}
	return []byte(svg), nil
}

func shiftFailure(err error) error {
	f := FailureOf(err)
	var line, column int
	if n, _ := fmt.Sscanf(f.Message, "the engine rejected the figure: Parse error at line %d column %d", &line, &column); n == 2 && line > nomnomlThemeLines {
		f.Span = Span{Line: line - nomnomlThemeLines, Column: column, EndLine: line - nomnomlThemeLines, EndColumn: column}
		f.Message = strings.Replace(f.Message, fmt.Sprintf("line %d", line), fmt.Sprintf("line %d", line-nomnomlThemeLines), 1)
	}
	return f
}

const advanceSize = 100

func advanceTable(source string) (string, error) {
	ruler, err := d2Ruler()
	if err != nil {
		return "", fail(FailureEngine, "label measurement failed to load its fonts: %v", err)
	}
	regular := d2fonts.SourceSansPro.Font(advanceSize, d2fonts.FONT_STYLE_REGULAR)
	bold := d2fonts.SourceSansPro.Font(advanceSize, d2fonts.FONT_STYLE_BOLD)
	seen := map[rune]bool{}
	b := []byte(`{"":[`)
	b = appendAdvance(b, ruler.Measure, regular, bold, "M")
	b = append(b, ']')
	for _, r := range source {
		if seen[r] || r == '\n' || r == utf8.RuneError {
			continue
		}
		seen[r] = true
		b = append(b, ',')
		b, _ = jsontext.AppendQuote(b, string(r))
		b = append(b, ":["...)
		b = appendAdvance(b, ruler.Measure, regular, bold, string(r))
		b = append(b, ']')
	}
	return string(append(b, '}')), nil
}

func appendAdvance(b []byte, measure func(d2fonts.Font, string) (int, int), regular, bold d2fonts.Font, s string) []byte {
	return fmt.Appendf(b, "%d,%d", advance(measure, regular, s), advance(measure, bold, s))
}

func advance(measure func(d2fonts.Font, string) (int, int), font d2fonts.Font, s string) int {
	with, _ := measure(font, "x"+s+"x")
	without, _ := measure(font, "xx")
	return max(with-without, 0)
}
