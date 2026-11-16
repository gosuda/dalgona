package tui

import (
	"context"
	_ "embed"
	"encoding/json/jsontext"
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/coreequip/mermaidascii"
	"modernc.org/quickjs"
	"oss.terrastruct.com/d2/d2graph"
	"oss.terrastruct.com/d2/d2layouts/d2dagrelayout"
	"oss.terrastruct.com/d2/d2lib"
	"oss.terrastruct.com/d2/d2renderers/d2ascii"
	"oss.terrastruct.com/d2/d2renderers/d2ascii/charset"
	"oss.terrastruct.com/d2/lib/textmeasure"
)

//go:embed nomnomljs/nomnoml.js
var nomnomlJS string

//go:embed nomnomljs/graphre.js
var graphreJS string

var diagramLangs = map[string]bool{
	"mermaid": true,
	"d2":      true,
	"nomnoml": true,
	"svg":     true,
}

const diagramBudget = 2 * time.Second

func isDiagramLang(lang string) bool {
	return diagramLangs[strings.ToLower(lang)]
}

func renderDiagram(kind, source string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), diagramBudget)
	defer cancel()
	switch kind {
	case "mermaid":
		return mermaidascii.Render(source)
	case "d2":
		return renderD2(ctx, source)
	case "nomnoml":
		return renderNomnoml(ctx, source)
	}
	return "", fmt.Errorf("no text renderer for %s", kind)
}

var d2Ruler = sync.OnceValues(textmeasure.NewRuler)

func renderD2(ctx context.Context, source string) (string, error) {
	ruler, err := d2Ruler()
	if err != nil {
		return "", err
	}
	diagram, _, err := d2lib.Compile(ctx, source, &d2lib.CompileOptions{
		Ruler: ruler,
		LayoutResolver: func(engine string) (d2graph.LayoutGraph, error) {
			if engine == "" || engine == "dagre" {
				return d2dagrelayout.DefaultLayout, nil
			}
			return nil, fmt.Errorf("d2 layout %q needs a renderer plugin", engine)
		},
	}, nil)
	if err != nil {
		return "", err
	}
	out, err := d2ascii.NewASCIIartist().Render(ctx, diagram, &d2ascii.RenderOpts{Charset: charset.Unicode})
	if err != nil {
		return "", err
	}
	return string(out), nil
}

const nomnomlShim = `
var module = { exports: {} };
var exports = module.exports;
var __deps = {};
function require(n) { return __deps[n] || {}; }
`

const nomnomlMid = `
__deps.graphre = module.exports;
module.exports = {};
exports = module.exports;
`

const nomnomlDraw = `
var __cells = {}, __maxC = 0, __maxR = 0, __cw = 6, __ch = 13;
function __put(px, py, ch) {
	var c = Math.floor(px / __cw), r = Math.floor(py / __ch);
	if (c < 0 || r < 0) { return; }
	__cells[r + ':' + c] = ch;
	if (c > __maxC) { __maxC = c; }
	if (r > __maxR) { __maxR = r; }
}
function __seg(x1, y1, x2, y2) {
	var dx = x2 - x1, dy = y2 - y1, steps = Math.max(Math.abs(dx), Math.abs(dy));
	if (steps < 1) { return; }
	var ch = Math.abs(dy) < Math.abs(dx) * 0.4 ? '-' : (Math.abs(dx) < Math.abs(dy) * 0.4 ? '|' : (dx * dy > 0 ? '\\' : '/'));
	for (var i = 0; i <= steps; i += 4) { __put(x1 + dx * i / steps, y1 + dy * i / steps, ch); }
}
function __arc(x, y, r, a0, a1) {
	var n = Math.max(8, r | 0);
	for (var i = 0; i <= n; i++) { var a = a0 + (a1 - a0) * i / n; __put(x + r * Math.cos(a), y + r * Math.sin(a), '.'); }
}
var ctx2d = {
	_tx: 0, _ty: 0, _st: [], _px: 0, _py: 0,
	font: '10pt sans-serif', fillStyle: '#000', strokeStyle: '#000',
	lineWidth: 1, lineCap: 'butt', lineJoin: 'miter', textAlign: 'left',
	setLineDash: function () {}, beginPath: function () {}, closePath: function () {},
	stroke: function () {}, fill: function () {}, clearRect: function () {},
	fillRect: function () {}, clip: function () {},
	save: function () { this._st.push([this._tx, this._ty]); },
	restore: function () { var s = this._st.pop(); if (s) { this._tx = s[0]; this._ty = s[1]; } },
	translate: function (x, y) { this._tx += x; this._ty += y; },
	scale: function () {},
	moveTo: function (x, y) { this._px = x; this._py = y; },
	lineTo: function (x, y) { __seg(this._tx + this._px, this._ty + this._py, this._tx + x, this._ty + y); this._px = x; this._py = y; },
	arc: function (x, y, r, a0, a1) { __arc(this._tx + x, this._ty + y, r, a0, a1); },
	arcTo: function (x1, y1, x2, y2) { __seg(this._tx + this._px, this._ty + this._py, this._tx + x1, this._ty + y1); this._px = x1; this._py = y1; },
	quadraticCurveTo: function (x1, y1, x2, y2) { __seg(this._tx + this._px, this._ty + this._py, this._tx + x2, this._ty + y2); this._px = x2; this._py = y2; },
	rect: function (x, y, w, h) { __seg(this._tx + x, this._ty + y, this._tx + x + w, this._ty + y); __seg(this._tx + x + w, this._ty + y, this._tx + x + w, this._ty + y + h); __seg(this._tx + x + w, this._ty + y + h, this._tx + x, this._ty + y + h); __seg(this._tx + x, this._ty + y + h, this._tx + x, this._ty + y); },
	measureText: function (t) { return { width: String(t).length * 6.5 }; },
	fillText: function (t, x, y) {
		var s = String(t), w = s.length * 6.5;
		if (this.textAlign === 'center') { x -= w / 2; } else if (this.textAlign === 'right') { x -= w; }
		for (var i = 0; i < s.length; i++) { __put(this._tx + x + i * 6.5, this._ty + y - 3, s[i]); }
	},
	canvas: null,
};
var nomnoml = module.exports;
var nomCanvas = { width: 4000, height: 4000, getContext: function () { return ctx2d; } };
ctx2d.canvas = nomCanvas;
nomnoml.draw(nomCanvas, %s, 1);
(function () {
	var rows = [];
	for (var r = 0; r <= __maxR; r++) {
		var row = '';
		for (var c = 0; c <= __maxC; c++) { row += __cells[r + ':' + c] || ' '; }
		rows.push(row.replace(/\s+$/, ''));
	}
	return rows.join('\n');
})()
`

func renderNomnoml(ctx context.Context, source string) (string, error) {
	vm, err := quickjs.NewVM()
	if err != nil {
		return "", err
	}
	defer vm.Close()
	vm.SetMaxStackSize(5000)
	vm.SetMemoryLimit(256 << 20)
	stop := context.AfterFunc(ctx, vm.Interrupt)
	defer stop()
	quoted, err := jsontext.AppendQuote(nil, source)
	if err != nil {
		return "", err
	}
	out, err := vm.Eval(nomnomlShim+graphreJS+nomnomlMid+nomnomlJS+"\n"+fmt.Sprintf(nomnomlDraw, quoted), quickjs.EvalGlobal)
	if err != nil {
		return "", err
	}
	text, ok := out.(string)
	if !ok {
		return "", fmt.Errorf("nomnoml returned %v", out)
	}
	return text, nil
}

func (m *model) renderDiagramBlock(b block) []string {
	art, err := renderDiagram(b.diagram, b.text)
	if err != nil {
		head := "diagram " + b.diagram + ": render failed · source shown"
		return append([]string{head}, strings.Split(b.text, "\n")...)
	}
	lines := strings.Split(strings.TrimRight(art, "\n"), "\n")
	return append([]string{"diagram " + b.diagram + " · " + strconv.Itoa(len(lines)) + " rows"}, lines...)
}
