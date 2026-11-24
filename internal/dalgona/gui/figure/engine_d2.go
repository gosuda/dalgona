package figure

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"strings"
	"sync"

	"oss.terrastruct.com/d2/d2ast"
	"oss.terrastruct.com/d2/d2compiler"
	"oss.terrastruct.com/d2/d2exporter"
	"oss.terrastruct.com/d2/d2graph"
	"oss.terrastruct.com/d2/d2layouts"
	"oss.terrastruct.com/d2/d2layouts/d2dagrelayout"
	"oss.terrastruct.com/d2/d2parser"
	"oss.terrastruct.com/d2/d2renderers/d2svg"
	"oss.terrastruct.com/d2/d2target"
	"oss.terrastruct.com/d2/d2themes/d2themescatalog"
	"oss.terrastruct.com/d2/lib/log"
	"oss.terrastruct.com/d2/lib/textmeasure"
)

type Theme struct {
	Canvas  Color
	Surface Color
	Tone    Color
	Line    Color
	Strong  Color
	Text    Color
	Dim     Color
	Accent  Color
}

func (c Color) hex() string { return fmt.Sprintf("#%02X%02X%02X", c.R, c.G, c.B) }

var d2Ruler = sync.OnceValues(textmeasure.NewRuler)

type refusedFS struct{}

func (refusedFS) Open(name string) (fs.File, error) {
	return nil, &fs.PathError{Op: "open", Path: name, Err: fs.ErrPermission}
}

func d2Overrides(t Theme) *d2target.ThemeOverrides {
	hex := func(c Color) *string { s := c.hex(); return &s }
	return &d2target.ThemeOverrides{
		N1: hex(t.Text), N2: hex(t.Dim), N3: hex(t.Strong), N4: hex(t.Line), N5: hex(t.Tone), N6: hex(t.Surface), N7: hex(t.Canvas),
		B1: hex(t.Strong), B2: hex(t.Strong), B3: hex(t.Line), B4: hex(t.Tone), B5: hex(t.Surface), B6: hex(t.Surface),
		AA2: hex(t.Accent), AA4: hex(t.Tone), AA5: hex(t.Surface), AB4: hex(t.Tone), AB5: hex(t.Surface),
	}
}

type engineOutput struct {
	svg      []byte
	overlays map[string]overlay
}

const d2ThemeID int64 = 0

type d2Request struct {
	source   string
	theme    Theme
	formulas formulaSource
}

func renderD2(ctx context.Context, r d2Request) (engineOutput, error) {
	source, theme := r.source, r.theme
	ast, err := d2parser.Parse("figure.d2", strings.NewReader(source), nil)
	if err != nil {
		return engineOutput{}, d2Failure(err)
	}
	spans, err := inspectD2(ast)
	if err != nil {
		return engineOutput{}, err
	}
	graph, _, err := d2compiler.Compile("figure.d2", strings.NewReader(source), &d2compiler.CompileOptions{FS: refusedFS{}})
	if err != nil {
		return engineOutput{}, d2Failure(err)
	}
	if len(graph.Objects) > GraphNodeLimit || len(graph.Edges) > GraphEdgeLimit {
		return engineOutput{}, fail(FailureLimit, "D2 figure has %d nodes and %d edges; the limit is %d nodes and %d edges", len(graph.Objects), len(graph.Edges), GraphNodeLimit, GraphEdgeLimit)
	}
	ruler, err := d2Ruler()
	if err != nil {
		return engineOutput{}, fail(FailureEngine, "D2 text measurement failed to load its fonts: %v", err)
	}
	labels := newRichLabels(ruler, spans, r.formulas)
	diagram, err := layoutD2(log.With(ctx, slog.New(slog.DiscardHandler)), graph, labels)
	if err != nil {
		return engineOutput{}, err
	}
	overrides := d2Overrides(theme)
	palette := d2themescatalog.Find(d2ThemeID)
	palette.ApplyOverrides(overrides)
	labels.place(diagram, palette)
	pad, themeID, omit := int64(16), d2ThemeID, true
	svg, err := d2svg.Render(diagram, &d2svg.RenderOpts{Pad: &pad, ThemeID: &themeID, ThemeOverrides: overrides, NoXMLTag: &omit, OmitVersion: &omit})
	if err != nil {
		return engineOutput{}, fail(FailureEngine, "D2 could not draw the figure: %v", err)
	}
	return engineOutput{svg: svg, overlays: labels.overlays()}, nil
}

func layoutD2(ctx context.Context, graph *d2graph.Graph, labels *richLabels) (*d2target.Diagram, error) {
	if err := graph.ApplyTheme(d2ThemeID); err != nil {
		return nil, fail(FailureEngine, "D2 theme failed: %v", err)
	}
	if err := labels.measure(graph); err != nil {
		return nil, err
	}
	if len(graph.Objects) > 0 {
		if err := graph.SetDimensions(labels.measured, labels.ruler, nil, nil); err != nil {
			return nil, d2Failure(err)
		}
		if err := d2layouts.LayoutNested(ctx, graph, d2layouts.NestedGraphInfo(graph.Root), d2dagrelayout.DefaultLayout, d2layouts.DefaultRouter); err != nil {
			return nil, d2Failure(err)
		}
	}
	diagram, err := d2exporter.Export(ctx, graph, nil, nil)
	if err != nil {
		return nil, fail(FailureEngine, "D2 could not export the figure: %v", err)
	}
	return diagram, nil
}

func d2Failure(err error) error {
	var parse *d2parser.ParseError
	if errors.As(err, &parse) && len(parse.Errors) > 0 {
		first := parse.Errors[0]
		return failAt(FailureInvalid, d2Span(first.Range), "%s", trimD2Location(first.Message))
	}
	return fail(FailureInvalid, "D2 rejected the figure: %v", err)
}

func trimD2Location(message string) string {
	if _, rest, ok := strings.Cut(message, ": "); ok && strings.HasPrefix(message, "figure.d2:") {
		return rest
	}
	return message
}

func d2Span(r d2ast.Range) Span {
	return Span{Line: r.Start.Line + 1, Column: r.Start.Column + 1, EndLine: r.End.Line + 1, EndColumn: r.End.Column + 1}
}

var d2RefusedKeys = map[string]string{
	"icon":         "D2 icons load remote or local resources and are refused; remove the icon",
	"fill-pattern": "D2 fill patterns are outside the native figure profile; remove fill-pattern",
}

var richTags = map[string]bool{"md": true, languageMarkdown: true, "tex": true, languageLaTeX: true}

func inspectD2(root *d2ast.Map) (map[string]d2ast.Range, error) {
	spans := map[string]d2ast.Range{}
	var refusal error
	d2ast.Walk(root, func(n d2ast.Node) bool {
		if block, ok := n.(*d2ast.BlockString); ok && richTags[block.Tag] {
			spans[block.Value] = block.Range
		}
		if refusal == nil {
			refusal = d2NodeRefusal(n)
		}
		return refusal == nil
	})
	return spans, refusal
}

func d2NodeRefusal(n d2ast.Node) error {
	switch v := n.(type) {
	case *d2ast.Import:
		return failAt(FailureForbidden, d2Span(v.Range), "D2 imports are refused; paste the imported content into the figure")
	case *d2ast.Key:
		return d2KeyRefusal(v)
	}
	return nil
}

type d2KeyRule struct {
	key     string
	code    FailureCode
	refuses func(value string) bool
	message func(value string) string
}

var d2ValueRefusals = []d2KeyRule{
	{
		key: "shape", code: FailureForbidden,
		refuses: func(v string) bool { return v == "image" },
		message: func(string) string { return "D2 image shapes load resources and are refused; use another shape" },
	},
	{
		key: "sketch", code: FailureInvalid,
		refuses: func(v string) bool { return v == "true" },
		message: func(string) string { return "D2 sketch mode is outside the native figure profile; remove sketch" },
	},
	{
		key: "layout-engine", code: FailureForbidden,
		refuses: func(v string) bool { return v != "dagre" },
		message: func(v string) string {
			return fmt.Sprintf("D2 layout engine %q is refused; native D2 figures use the bundled dagre layout", v)
		},
	},
}

func d2KeyValue(k *d2ast.Key) (string, string, bool) {
	if k.Key == nil || len(k.Key.Path) == 0 {
		return "", "", false
	}
	value := ""
	if scalar := k.Value.ScalarBox().Unbox(); scalar != nil {
		value = scalar.ScalarString()
	}
	return k.Key.Last().Unbox().ScalarString(), value, true
}

func d2KeyRefusal(k *d2ast.Key) error {
	name, value, ok := d2KeyValue(k)
	if !ok {
		return nil
	}
	if message, refused := d2RefusedKeys[name]; refused {
		return failAt(FailureForbidden, d2Span(k.Range), "%s", message)
	}
	for _, rule := range d2ValueRefusals {
		if name == rule.key && rule.refuses(value) {
			return failAt(rule.code, d2Span(k.Range), "%s", rule.message(value))
		}
	}
	return nil
}
