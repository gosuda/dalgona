package tui

import (
	"context"
	_ "embed"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"log/slog"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/coreequip/mermaidascii"
	"modernc.org/quickjs"
	"oss.terrastruct.com/d2/d2graph"
	"oss.terrastruct.com/d2/d2layouts/d2dagrelayout"
	"oss.terrastruct.com/d2/d2lib"
	"oss.terrastruct.com/d2/d2renderers/d2ascii"
	"oss.terrastruct.com/d2/d2renderers/d2ascii/charset"
	"oss.terrastruct.com/d2/lib/log"
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

var diagramBudget = 2 * time.Second

const diagramSourceLimit = 16 << 10

var errDiagramTooLarge = errors.New("too large to draw")

func isDiagramLang(lang string) bool {
	return diagramLangs[strings.ToLower(lang)]
}

type drawFunc func(ctx context.Context, kind, source string) (string, error)

type diagramMsg struct {
	key   string
	lines []string
}

func drawDiagram(ctx context.Context, kind, source string) (string, error) {
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

func renderDiagramWith(draw drawFunc, kind, source string) (string, error) {
	if len(source) > diagramSourceLimit {
		return "", errDiagramTooLarge
	}
	ctx, cancel := context.WithTimeout(context.Background(), diagramBudget)
	defer cancel()
	type drawn struct {
		art string
		err error
	}
	done := make(chan drawn, 1)
	go func() {
		defer func() {
			if r := recover(); r != nil {
				done <- drawn{err: fmt.Errorf("render panic: %v", r)}
			}
		}()
		art, err := draw(ctx, kind, source)
		done <- drawn{art: art, err: err}
	}()
	select {
	case result := <-done:
		return result.art, result.err
	case <-ctx.Done():
		return "", ctx.Err()
	}
}

var errD2Busy = errors.New("a d2 layout past its limit still runs")

type d2Gate struct {
	slot chan struct{}
	due  atomic.Int64
}

func newD2Gate() *d2Gate {
	return &d2Gate{slot: make(chan struct{}, 1)}
}

func (g *d2Gate) enter(ctx context.Context) (func(), error) {
	if due := g.due.Load(); due != 0 && time.Now().UnixNano() >= due {
		return nil, errD2Busy
	}
	select {
	case g.slot <- struct{}{}:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	if deadline, ok := ctx.Deadline(); ok {
		g.due.Store(deadline.UnixNano())
	}
	return func() {
		g.due.Store(0)
		<-g.slot
	}, nil
}

var (
	d2Ruler   = sync.OnceValues(textmeasure.NewRuler)
	d2Layouts = newD2Gate()
)

func renderD2(ctx context.Context, source string) (string, error) {
	leave, err := d2Layouts.enter(ctx)
	if err != nil {
		return "", err
	}
	defer leave()
	if err = ctx.Err(); err != nil {
		return "", err
	}
	ctx = log.With(ctx, slog.New(slog.DiscardHandler))
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

type heldCommit struct {
	key     string
	lines   []string
	waiting bool
}

func (m *model) commitDiagram(key string, b block) {
	if !m.keepEntry(key, diagramSourceCard("diagram "+b.diagram+" · drawing", b)) {
		return
	}
	m.out.held = append(m.out.held, heldCommit{key: key, waiting: true})
	m.out.launch = append(m.out.launch, m.renderDiagramBlock(key, b))
}

func (m *model) renderDiagramBlock(key string, b block) tea.Cmd {
	draw := m.out.draw
	if draw == nil {
		draw = drawDiagram
	}
	return func() tea.Msg {
		return diagramMsg{key: key, lines: diagramLines(draw, b)}
	}
}

func diagramLines(draw drawFunc, b block) []string {
	art, err := renderDiagramWith(draw, b.diagram, b.text)
	switch {
	case errors.Is(err, errDiagramTooLarge):
		return diagramSourceCard("diagram "+b.diagram+": too large to draw · source shown", b)
	case err != nil:
		return diagramSourceCard("diagram "+b.diagram+": render failed · source shown", b)
	}
	lines := strings.Split(strings.TrimRight(art, "\n"), "\n")
	return append([]string{"diagram " + b.diagram + " · " + strconv.Itoa(len(lines)) + " rows"}, lines...)
}

func diagramSourceCard(head string, b block) []string {
	return append([]string{head}, strings.Split(b.text, "\n")...)
}

func (m *model) takeDiagramCommands() tea.Cmd {
	if len(m.out.launch) == 0 {
		return nil
	}
	command := tea.Batch(m.out.launch...)
	m.out.launch = nil
	return command
}

func (m *model) applyDiagram(msg diagramMsg) {
	i := slices.IndexFunc(m.out.held, func(h heldCommit) bool { return h.waiting && h.key == msg.key })
	if i < 0 {
		return
	}
	m.out.held[i] = heldCommit{lines: msg.lines}
	if j := slices.IndexFunc(m.out.entries, func(e entry) bool { return e.key == msg.key }); j >= 0 {
		m.out.entries[j] = newEntry(msg.key, msg.lines)
		if m.review != nil {
			m.fitReview()
		}
	}
	ready := 0
	for ready < len(m.out.held) && !m.out.held[ready].waiting {
		m.out.pending = append(m.out.pending, m.out.held[ready].lines...)
		ready++
	}
	m.out.held = slices.Delete(m.out.held, 0, ready)
}

func (m *model) heldLines() int {
	n := 0
	for _, held := range m.out.held {
		n += len(held.lines)
	}
	return n
}
