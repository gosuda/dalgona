//go:build gui

package transcript

import (
	"context"
	"errors"
	"image"
	"strconv"
	"sync"
	"time"

	"gioui.org/f32"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	gmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	factCacheLimit = 4096
	multiClick     = 400 * time.Millisecond
	busyPoll       = 50 * time.Millisecond
	dragFrame      = 16 * time.Millisecond
)

var errResident = errors.New("transcript documents stay resident; nothing to page")

type factKey struct {
	item     string
	revision string
	kind     string
}
type Transcript struct {
	deps             Deps
	loader           *loader
	panes            map[state.ThreadKey]*pane
	facts            map[factKey]facts
	geometry         *document.Cache[*document.RowLayout]
	level            Level
	focus            struct{ _ byte }
	jump             widget.Clickable
	review           *reviewer
	notice           string
	env              shell.Env
	palette          theme.Palette
	ctrl             control
	waiting          map[state.ItemKey]bool
	timedOut         map[state.ItemKey]bool
	targets          map[state.RequestKey]requestTarget
	figures          map[document.Key]*figureState
	formulas         map[formulaKey]*gmath.Layout
	exports          exporter
	linkMu           sync.Mutex
	linkGen          uint64
	pendingImageCopy *pendingImageCopy
	linkPending      *linkResult
	figureClock      uint64
	pendingCopy      *pendingCopy
}

func New(deps Deps) *Transcript {
	return &Transcript{
		deps:     deps,
		loader:   newLoader(),
		panes:    map[state.ThreadKey]*pane{},
		facts:    map[factKey]facts{},
		geometry: document.NewCache[*document.RowLayout](document.GeometryBudget),
		waiting:  map[state.ItemKey]bool{},
		timedOut: map[state.ItemKey]bool{},
		targets:  map[state.RequestKey]requestTarget{},
	}
}

func (t *Transcript) Close() {
	t.exports.cancel()
	for _, p := range t.panes {
		for _, r := range p.rows {
			t.releaseImages(r)
		}
	}
	clear(t.panes)
	t.linkMu.Lock()
	t.linkGen++
	t.linkPending = nil
	t.linkMu.Unlock()
	t.pendingCopy = nil
	if t.review != nil {
		t.review.cancelJobs()
		t.review.bridge.abort()
	}
	if t.loader != nil {
		t.releaseWindowSources()
	}
}

func (t *Transcript) FocusTag() event.Tag { return &t.focus }

type pressState struct {
	at       time.Time
	where    f32.Point
	count    int
	drag     bool
	dragAt   f32.Point
	dragTime time.Time
	link     document.Link
	linked   bool
}

type pane struct {
	key              state.ThreadKey
	scope            *document.Scope
	sel              *document.State
	vp               *document.Viewport
	rows             map[document.Identity]*row
	order            []*row
	expanded         map[document.Identity]bool
	folding          []document.Fold
	frame            uint64
	placed           []placed
	accessible       []accessibleRow
	accessPages      map[accessRange]*accessPage
	accessReads      int
	accessNavigation accessNavigation
	accessGroup      accessRange
	focusLast        bool
	lastTag          struct{ _ byte }
	press            pressState
	goal             float32
	goalSet          bool
	revealCaret      bool
	diffJump         *diffJump
	seen             map[string]string
	announced        map[string]bool
	loading          bool
	pxPerDp          float32
	scrolled         float64
	pinTop           bool
}

func newPane(key state.ThreadKey) *pane {
	scope := document.NewScope(string(key.Endpoint), key.Thread)
	return &pane{
		key: key, scope: scope, sel: document.NewState(scope), vp: document.NewViewport(),
		rows: map[document.Identity]*row{}, expanded: map[document.Identity]bool{},
		seen: map[string]string{}, announced: map[string]bool{},
	}
}

func (t *Transcript) paneFor(key state.ThreadKey) *pane {
	p, ok := t.panes[key]
	if !ok {
		p = newPane(key)
		t.panes[key] = p
	}
	return p
}

func (t *Transcript) factsOf(it state.ItemView) facts {
	key := factKey{item: it.Key.Item, revision: it.Header.ItemRevision, kind: it.Header.Type}
	if f, ok := t.facts[key]; ok && key.revision != "" {
		return f
	}
	if len(t.facts) >= factCacheLimit {
		clear(t.facts)
	}
	f := readFacts(it.Header)
	t.facts[key] = f
	return f
}

func (t *Transcript) verbosity(env shell.Env) Level {
	for _, ep := range env.View.Endpoints() {
		if ep.ID == env.Thread.Endpoint {
			return ParseLevel(ep.Verbosity)
		}
	}
	return LevelMedium
}

func (t *Transcript) Layout(gtx layout.Context, env shell.Env) layout.Dimensions {
	if env.Thread != t.env.Thread {
		t.cancelImageCopy()
		if previous := t.panes[t.env.Thread]; previous != nil {
			previous.accessNavigation = accessNavigation{}
			previous.press = pressState{}
		}
	}
	defer func() { t.env.View = nil }()
	t.env, t.palette = env, env.Theme.Palette()
	t.ctrl = control{shaper: env.Shaper, palette: t.palette}
	t.loader.begin(env.View)
	t.level = t.verbosity(env)
	size := gtx.Constraints.Max
	defer clip.Rect{Max: size}.Push(gtx.Ops).Pop()
	event.Op(gtx.Ops, &t.focus)
	tv, ok := env.View.Thread(env.Thread)
	if !ok {
		t.cancelImageCopy()
		t.layoutReview(gtx, env, nil)
		t.trimSources(gtx, nil)
		t.drainKeys(gtx)
		return layout.Dimensions{Size: size}
	}
	p := t.paneFor(env.Thread)
	p.frame++
	p.pxPerDp = max(gtx.Metric.PxPerDp, 0.01)
	t.trackRequests(tv)
	t.trackResolved(tv)
	if status, done := t.exports.take(); done {
		t.notice = status
	}
	t.drainLinks()
	t.announce(env, tv, p)
	t.figureClock++
	rows := t.desired(tv, p, env.View)
	t.layoutInput(gtx, env, p)
	t.prepareSheets(p, rows)
	t.layoutTranscript(gtx, env, tv, p, rows)
	if t.loader.pump(env.View, env.Submit) || t.review.busy() {
		gtx.Execute(op.InvalidateCmd{At: gtx.Now.Add(busyPoll)})
	}
	t.sweepFigures()
	t.layoutReview(gtx, env, tv)
	t.settleCopy(gtx)
	t.trimSources(gtx, p)
	return layout.Dimensions{Size: size}
}

func (t *Transcript) layoutInput(gtx layout.Context, env shell.Env, p *pane) {
	if t.review.active() || env.Modal {
		return
	}
	t.handleKeys(gtx, env, p)
	if !t.review.active() {
		t.pointerFor(gtx, p, &t.focus)
	}
}

func (t *Transcript) layoutTranscript(gtx layout.Context, env shell.Env, tv *state.ThreadView, p *pane, rows []*row) {
	covered := t.review.active() || env.Modal
	if covered {
		p.press = pressState{}
		gtx = access.Hide(gtx.Disabled())
	}
	t.semanticPane(gtx, env, p, rows)
	if !covered {
		p.pruneSourceRanges()
		t.olderOnTop(env, tv, p)
	}
	if len(p.order) == 0 {
		t.layoutEmpty(gtx, tv)
	}
	t.layoutJump(gtx, p)
	t.finishImageCopy(p)
	t.layoutNotice(gtx)
}

func (t *Transcript) drainKeys(gtx layout.Context) {
	for {
		if _, ok := gtx.Event(key.FocusFilter{Target: &t.focus}); !ok {
			return
		}
	}
}

func (p *pane) reconcile(rows []*row) bool {
	rebuild := len(rows) != len(p.order)
	for i := 0; !rebuild && i < len(rows); i++ {
		rebuild = rows[i] != p.order[i]
	}
	changed := rebuild
	for _, r := range rows {
		fresh, updated := r.refresh(p.sel)
		rebuild = rebuild || fresh
		changed = changed || updated
	}
	if rebuild {
		p.rebuild(rows)
	}
	return changed
}

func (p *pane) rebuild(rows []*row) {
	scope := document.NewScope(string(p.key.Endpoint), p.key.Thread)
	kept := make([]*row, 0, len(rows))
	for _, r := range rows {
		if r.doc == nil {
			continue
		}
		if _, err := scope.Put(r.doc); err == nil {
			kept = append(kept, r)
		}
	}
	next := document.NewState(scope)
	carryFolds(p.scope, next, p.vp, kept)
	p.carrySelection(next)
	p.scope, p.sel, p.order = scope, next, kept
}

func (p *pane) carrySelection(next *document.State) {
	scope := next.Scope
	next.Pending = p.sel.Pending
	anchor := p.sel.Selection.Anchor.Document
	focus := p.sel.Selection.Focus.Document
	_, anchorPresent := scope.Index(anchor)
	_, focusPresent := scope.Index(focus)
	if p.sel.Selected && anchorPresent && focusPresent {
		next.Selection, next.Selected = p.sel.Selection, true
	}
	if _, present := scope.Index(p.sel.Anchor.Position.Document); p.sel.Anchored && present {
		next.Anchor, next.Anchored = p.sel.Anchor, true
	}
}

func (p *pane) estimate(ref document.RowRef, _ *document.Scope) float32 {
	h := float32(theme.SizeBody) * theme.LeadingBody
	if ref.Block <= 0 && ref.Row == 0 {
		h += float32(theme.Space6 + theme.MetaHeight)
	}
	return h
}

func zeroTop(int, document.Position) float32 { return 0 }

func (p *pane) captureAnchor() {
	if p.vp.Following() || p.vp.Rows() == 0 {
		return
	}
	i := p.vp.RowAt(p.vp.Offset())
	ref := p.vp.Row(i)
	pos := p.scope.Edge(ref.Entry, false)
	if doc, ok := p.scope.Loaded(ref.Entry); ok && ref.Block >= 0 && ref.Block < doc.Len() {
		if at, err := doc.At(ref.Block, doc.Block(ref.Block).Rows[ref.Row].Start, document.After); err == nil {
			pos = at
		}
	}
	p.sel.Anchor, p.sel.Anchored = p.vp.Capture(i, pos, 0), true
}

type laid struct {
	call op.CallOp
	info placed
}

type placed struct {
	index     int
	ref       document.RowRef
	row       *row
	y         int
	last      bool
	height    int
	text      image.Point
	layout    *document.RowLayout
	split     *splitLayout
	block     int
	focusTags []event.Tag
}

func (pl placed) size() image.Point {
	if pl.split != nil {
		return image.Pt(pl.split.width, pl.split.height)
	}
	if pl.layout == nil {
		return image.Point{}
	}
	return pl.layout.Size()
}

func (pl placed) flow() *document.Flow {
	if pl.split != nil {
		return pl.split.flow()
	}
	if pl.layout == nil {
		return nil
	}
	return &pl.layout.Flow
}

func (pl placed) hit(local f32.Point) (document.Position, bool) {
	var offset int
	var affinity document.Affinity
	if pl.split != nil {
		offset, affinity = pl.split.hit(local)
	} else {
		offset, affinity = pl.layout.Hit(local)
	}
	pos, err := pl.row.doc.At(pl.block, offset, affinity)
	return pos, err == nil
}

func (t *Transcript) layoutRows(gtx layout.Context, env shell.Env, p *pane, rows []*row) {
	size := gtx.Constraints.Max
	p.vp.Resize(float64(size.Y) / float64(p.pxPerDp))
	if p.scrolled != 0 {
		p.vp.ScrollBy(p.scrolled)
		p.scrolled, p.pinTop = 0, false
	}
	p.captureAnchor()
	folded := p.applyFolds()
	if p.reconcile(rows) || folded || p.vp.Rows() == 0 {
		p.vp.Sync(p.scope, p.estimate)
		clear(p.accessible)
		p.accessible = p.accessible[:0]
	}
	p.restore()
	p.seekDiff()
	t.scrollDrag(gtx, p)
	if p.revealCaret {
		t.scrollCaret(gtx, env, p, nil)
	}
	if len(p.order) == 0 {
		p.placed = p.placed[:0]
		return
	}
	done := t.measure(gtx, env, p)
	t.paint(gtx, p, done)
	t.selectDragEdge(gtx, p)
	p.revealCaret = false
	t.registerRows(gtx, p, done)
}

func (t *Transcript) measure(gtx layout.Context, env shell.Env, p *pane) map[int]laid {
	done := map[int]laid{}
	for range 3 {
		top := p.vp.Offset()
		remeasured := t.measureWindow(gtx, env, p, done)
		if remeasured {
			p.restore()
		}
		if p.revealCaret {
			t.scrollCaret(gtx, env, p, done)
		}
		if !remeasured && top == p.vp.Offset() {
			break
		}
	}
	return done
}

func (t *Transcript) measureWindow(gtx layout.Context, env shell.Env, p *pane, done map[int]laid) bool {
	first, last := p.vp.Window()
	remeasured := false
	for i := first; i < last; i++ {
		if _, ok := done[i]; ok {
			continue
		}
		macro := op.Record(gtx.Ops)
		info := t.layoutRow(gtx, env, p, i)
		done[i] = laid{call: macro.Stop(), info: info}
		height := float32(info.height) / p.pxPerDp
		if diff := height - p.vp.Height(i); diff > 0.5 || diff < -0.5 {
			p.vp.Measure(i, height)
			remeasured = true
		}
	}
	return remeasured
}

func (p *pane) restore() {
	p.vp.Restore(p.scope, p.sel.Anchor, zeroTop)
	if p.pinTop {
		p.vp.ScrollBy(-p.vp.Total())
	}
}

func (t *Transcript) paint(gtx layout.Context, p *pane, done map[int]laid) {
	p.placed = p.placed[:0]
	first, last := p.vp.Window()
	top := p.vp.Offset()
	for i := first; i < last; i++ {
		l, ok := done[i]
		if !ok {
			continue
		}
		y := int((p.vp.RowTop(i) - top) * float64(p.pxPerDp))
		stack := op.Offset(image.Pt(0, y)).Push(gtx.Ops)
		l.call.Add(gtx.Ops)
		stack.Pop()
		l.info.y = y
		p.placed = append(p.placed, l.info)
	}
	if p.focusLast {
		if _, ok := done[p.vp.Rows()-1]; ok {
			gtx.Execute(key.FocusCmd{Tag: &p.lastTag})
			p.focusLast = false
		}
	}
}

func (t *Transcript) olderOnTop(env shell.Env, tv *state.ThreadView, p *pane) {
	if tv.HistoryComplete || tv.LoadingOlder || p.vp.Offset() > 0 || len(tv.Turns) == 0 {
		return
	}
	env.Submit(state.LoadOlder{Thread: tv.Key})
}

func (t *Transcript) layoutEmpty(gtx layout.Context, tv *state.ThreadView) {
	text := "Thread ready. Start with the outcome: describe the task, the constraints, and what the agent should verify."
	if tv.Opening || !tv.HistoryComplete {
		text = "Loading transcript"
	}
	inset := layout.UniformInset(unit.Dp(theme.Space8))
	inset.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		return label(gtx, t.env.Shaper, prose(t.palette.Dim), text)
	})
}

func (t *Transcript) layoutJump(gtx layout.Context, p *pane) {
	if p.vp.Following() {
		return
	}
	if t.jump.Clicked(gtx) {
		p.vp.JumpToLatest()
		p.sel.Anchored = false
		p.focusLast = true
		return
	}
	name := "Jump to latest"
	if n := p.vp.Unseen(); n > 0 {
		name += " · " + itoa(n) + " new"
	}
	macro := op.Record(gtx.Ops)
	child := gtx
	child.Constraints.Min = image.Point{}
	dims := t.ctrl.button(child, &t.jump, name, false)
	call := macro.Stop()
	size := gtx.Constraints.Max
	at := image.Pt((size.X-dims.Size.X)/2, size.Y-dims.Size.Y-dp(gtx, theme.Space4))
	stack := op.Offset(at).Push(gtx.Ops)
	call.Add(gtx.Ops)
	stack.Pop()
}

func (t *Transcript) layoutNotice(gtx layout.Context) {
	if t.notice == "" {
		return
	}
	macro := op.Record(gtx.Ops)
	child := gtx
	child.Constraints.Min = image.Point{}
	dims := label(child, t.env.Shaper, caption(t.palette.Dim), t.notice)
	call := macro.Stop()
	size := gtx.Constraints.Max
	stack := op.Offset(image.Pt(dp(gtx, theme.Space4), size.Y-dims.Size.Y-dp(gtx, theme.Space1))).Push(gtx.Ops)
	call.Add(gtx.Ops)
	stack.Pop()
}

func (t *Transcript) announce(env shell.Env, tv *state.ThreadView, p *pane) {
	loading := tv.Opening || (len(tv.Turns) == 0 && !tv.HistoryComplete)
	if loading && !p.loading {
		env.Announce(platform.Announcement{Kind: platform.AnnounceProgress, Text: "Loading transcript"})
	}
	p.loading = loading
	if len(tv.Turns) == 0 {
		return
	}
	p.announceTurn(env, tv.Turns[len(tv.Turns)-1])
}

func (p *pane) announceTurn(env shell.Env, turn state.TurnView) {
	if before, ok := p.seen[turn.ID]; ok && before != turn.Status {
		if n, ok := turnAnnouncement(turn); ok {
			env.Announce(n)
		}
	}
	p.seen[turn.ID] = turn.Status
	if turn.RetryError != nil && turn.Status == rpc.StatusInProgress {
		p.once(env, "retry\x00"+turn.ID+"\x00"+turn.RetryError.Message, platform.Announcement{Kind: platform.AnnounceProgress, Text: "Retrying after an error: " + turn.RetryError.Message})
	}
	p.announceReviews(env, turn)
}

func (p *pane) announceReviews(env shell.Env, turn state.TurnView) {
	if turn.Review != nil && turn.Review.Summary != "" {
		p.once(env, "review\x00"+turn.ID+"\x00"+turn.Review.Summary, platform.Announcement{Kind: platform.AnnounceCompletion, Text: turn.Review.Summary})
	}
	for _, it := range turn.Items {
		if it.Review != nil && it.Review.Summary != "" {
			p.once(env, "review\x00"+turn.ID+"\x00"+it.Key.Item+"\x00"+it.Review.Summary, platform.Announcement{Kind: platform.AnnounceCompletion, Text: it.Review.Summary})
		}
	}
}

func (p *pane) once(env shell.Env, key string, n platform.Announcement) {
	if p.announced[key] {
		return
	}
	p.announced[key] = true
	env.Announce(n)
}

func turnAnnouncement(turn state.TurnView) (platform.Announcement, bool) {
	switch turn.Status {
	case rpc.StatusCompleted:
		return platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Done"}, true
	case rpc.StatusFailed:
		text := "Failed"
		if turn.Error != nil {
			text += ": " + turn.Error.Message
		}
		return platform.Announcement{Kind: platform.AnnounceFailure, Text: text}, true
	case "interrupted":
		return platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Stopped"}, true
	}
	return platform.Announcement{}, false
}

func (t *Transcript) handleKeys(gtx layout.Context, env shell.Env, p *pane) {
	filters := []event.Filter{
		key.FocusFilter{Target: &t.focus},
		key.Filter{Focus: &t.focus, Name: "C", Required: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: "A", Required: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NamePageUp},
		key.Filter{Focus: &t.focus, Name: key.NamePageDown},
		key.Filter{Focus: &t.focus, Name: key.NameLeftArrow, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameRightArrow, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameReturn},
		key.Filter{Focus: &t.focus, Name: key.NameEnter},
		key.Filter{Focus: &t.focus, Name: key.NameUpArrow},
		key.Filter{Focus: &t.focus, Name: key.NameDownArrow},
		key.Filter{Focus: &t.focus, Name: key.NameHome, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameEnd, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameLeftArrow, Required: key.ModShift, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameRightArrow, Required: key.ModShift, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameUpArrow, Required: key.ModShift},
		key.Filter{Focus: &t.focus, Name: key.NameDownArrow, Required: key.ModShift},
		key.Filter{Focus: &t.focus, Name: key.NameHome, Required: key.ModShift, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameEnd, Required: key.ModShift, Optional: key.ModShortcut},
		key.Filter{Focus: &t.focus, Name: key.NameEscape},
	}
	if !env.Modal && !t.review.active() {
		filters = append(filters, key.Filter{Name: "O", Required: key.ModShortcut})
	}
	for {
		ev, ok := gtx.Event(filters...)
		if !ok {
			return
		}
		if e, ok := ev.(key.Event); ok && e.State == key.Press {
			t.key(gtx, env, p, e)
		}
	}
}

func (t *Transcript) key(gtx layout.Context, env shell.Env, p *pane, e key.Event) {
	if e.Modifiers.Contain(key.ModShift) {
		t.extendKey(gtx, env, p, e)
		return
	}
	switch e.Name { //nolint:exhaustive // key.Name includes arbitrary printable key names.
	case "O":
		t.openLatest(gtx, p)
	case "C":
		t.copySelection(gtx, p)
	case "A":
		p.selectAll()
	case key.NameReturn, key.NameEnter:
		t.openCaretLink(p)
	case key.NameEscape:
		p.sel.Selected = false
	case key.NameLeftArrow, key.NameRightArrow:
		p.moveCaret(e.Name == key.NameRightArrow, e.Modifiers.Contain(key.ModShortcut))
	default:
		page := float64(gtx.Constraints.Max.Y) / float64(p.pxPerDp) * 0.9
		p.scrolled += scrollFor(e.Name, page, p.vp.Total())
	}
}

func (p *pane) selectAll() {
	if all, err := p.scope.All(); err == nil {
		p.sel.Selection, p.sel.Selected = all, true
	}
}

func scrollFor(name key.Name, page, total float64) float64 {
	switch name { //nolint:exhaustive // key.Name includes arbitrary printable key names
	case key.NamePageUp:
		return -page
	case key.NamePageDown:
		return page
	case key.NameUpArrow:
		return -float64(theme.RequestScrollStep)
	case key.NameDownArrow:
		return float64(theme.RequestScrollStep)
	case key.NameHome:
		return -total
	case key.NameEnd:
		return total
	}
	return 0
}

func (t *Transcript) copySelection(gtx layout.Context, p *pane) {
	if !p.sel.Selected || p.sel.Selection.Collapsed() {
		return
	}
	if t.copySpilledSelection(gtx, p) {
		return
	}
	if t.copyComplete(p, p.sel.Selection) {
		t.writeCopy(gtx, p, p.sel.Selection)
		return
	}
	if !t.armCopy(p, p.sel.Selection) {
		t.pendingCopy = nil
		t.notice = "Copy failed: the selection is not fully loaded; open Full text to copy it all."
		return
	}
	t.pendingCopy = &pendingCopy{pane: p, all: coversAll(p, p.sel.Selection)}
	t.notice = "Loading the full selection for copy."
}

func coversAll(p *pane, sel document.Selection) bool {
	n := p.scope.Len()
	if n == 0 {
		return false
	}
	head := p.scope.Edge(0, false)
	tail := p.scope.Edge(n-1, true)
	return covers(head, tail, sel) || covers(tail, head, sel)
}

func covers(a, b document.Position, sel document.Selection) bool {
	return a.Document == sel.Anchor.Document && a.Offset == sel.Anchor.Offset &&
		b.Document == sel.Focus.Document && b.Offset == sel.Focus.Offset
}

type pendingCopy struct {
	pane *pane
	all  bool
}

func (t *Transcript) copyBacking(p *pane, sel document.Selection) ([]*body, []*viewField, bool) {
	a, ok := p.scope.Index(sel.Anchor.Document)
	if !ok {
		return nil, nil, false
	}
	b, ok := p.scope.Index(sel.Focus.Document)
	if !ok {
		return nil, nil, false
	}
	var bodies []*body
	var fields []*viewField
	for i := min(a, b); i <= max(a, b); i++ {
		id := p.scope.Ref(i).Header.Identity
		r, ok := p.rows[id]
		if !ok {
			return nil, nil, false
		}
		if r.field != nil {
			fields = append(fields, r.field)
			continue
		}
		bodies = append(bodies, r.sheet.sources...)
	}
	return bodies, fields, true
}

func (t *Transcript) copyComplete(p *pane, sel document.Selection) bool {
	bodies, fields, ok := t.copyBacking(p, sel)
	if !ok {
		return false
	}
	for _, b := range bodies {
		if !b.complete() || b.start != 0 {
			return false
		}
	}
	for _, f := range fields {
		if !f.complete() || f.base != 0 {
			return false
		}
	}
	return true
}

func (t *Transcript) armCopy(p *pane, sel document.Selection) bool {
	bodies, fields, ok := t.copyBacking(p, sel)
	if !ok {
		return false
	}
	for _, b := range bodies {
		switch {
		case b.err != nil:
			t.loader.retry(b, t.env.Submit)
		case !b.complete() || b.start != 0:
			t.loader.promote(b)
		}
	}
	for _, f := range fields {
		if f.base != 0 {
			f.reset()
		}
		f.pull = true
	}
	return true
}

func (t *Transcript) copyFailed(p *pane, sel document.Selection) bool {
	bodies, fields, ok := t.copyBacking(p, sel)
	if !ok {
		return true
	}
	for _, b := range bodies {
		if b.err != nil {
			return true
		}
	}
	for _, f := range fields {
		if f.err != nil {
			return true
		}
	}
	return false
}

func (t *Transcript) settleCopy(gtx layout.Context) {
	pc := t.pendingCopy
	if pc == nil {
		return
	}
	if t.review != nil && t.review.spill == pc.pane {
		return
	}
	p := pc.pane
	sel, ok := t.parkedSelection(p, pc)
	if !ok {
		t.pendingCopy = nil
		return
	}
	if !t.copyComplete(p, sel) {
		if t.copyFailed(p, sel) {
			t.pendingCopy = nil
			t.notice = "Copy failed: the selection is not fully loaded; open Full text to copy it all."
			return
		}
		gtx.Execute(op.InvalidateCmd{At: gtx.Now.Add(busyPoll)})
		return
	}
	if t.sheetWaiting(p, sel) {
		gtx.Execute(op.InvalidateCmd{At: gtx.Now.Add(busyPoll)})
		return
	}
	t.pendingCopy = nil
	t.writeCopy(gtx, p, sel)
}

func (t *Transcript) sheetWaiting(p *pane, sel document.Selection) bool {
	a, ok := p.scope.Index(sel.Anchor.Document)
	if !ok {
		return false
	}
	b, ok := p.scope.Index(sel.Focus.Document)
	if !ok {
		return false
	}
	for i := min(a, b); i <= max(a, b); i++ {
		if r, ok := p.rows[p.scope.Ref(i).Header.Identity]; ok && r.sheet.wait {
			return true
		}
	}
	return false
}

func (t *Transcript) parkedSelection(p *pane, pc *pendingCopy) (document.Selection, bool) {
	if !p.sel.Selected {
		return document.Selection{}, false
	}
	if !pc.all {
		if p.sel.Selection.Collapsed() {
			return document.Selection{}, false
		}
		return p.sel.Selection, true
	}
	if all, err := p.scope.All(); err == nil {
		return all, true
	}
	if p.sel.Selection.Collapsed() {
		return document.Selection{}, false
	}
	return p.sel.Selection, true
}

func (t *Transcript) writeCopy(gtx layout.Context, p *pane, sel document.Selection) {
	if err := p.scope.CopyText(context.Background(), gtx.Source, sel, residentPager{}); err != nil {
		t.notice = "Copy failed: " + err.Error()
		return
	}
	t.notice = "Copied the selection."
}

type residentPager struct{}

func (residentPager) Page(context.Context, document.Ref, int, int) (string, error) {
	return "", errResident
}

func (t *Transcript) extendKey(gtx layout.Context, env shell.Env, p *pane, e key.Event) {
	if !p.sel.Selected {
		return
	}
	far := e.Modifiers.Contain(key.ModShortcut)
	flow := func(i int) (*document.Flow, bool) {
		f, _, ok := t.navigationLayout(gtx, env, p, i, nil)
		return f, ok
	}
	before := p.sel.Selection.Focus
	switch e.Name { //nolint:exhaustive // key.Name includes arbitrary printable key names
	case key.NameUpArrow, key.NameDownArrow:
		p.extendLine(e.Name == key.NameDownArrow, flow)
	case key.NameHome, key.NameEnd:
		p.extendEdge(e.Name == key.NameEnd, far, flow)
	default:
		p.extend(e.Name == key.NameRightArrow, far)
	}
	if e.Name != key.NameUpArrow && e.Name != key.NameDownArrow {
		p.goalSet = false
	}
	p.revealCaret = p.revealCaret || p.sel.Selection.Focus != before
}

func (t *Transcript) scrollCaret(gtx layout.Context, env shell.Env, p *pane, measured map[int]laid) {
	focus := p.sel.Selection.Focus
	i, ok := p.vp.Locate(p.scope, focus)
	if !ok {
		return
	}
	y := p.vp.RowTop(i)
	end := y + float64(p.vp.Height(i))
	if flow, offset, ok := t.navigationLayout(gtx, env, p, i, measured); ok {
		if s, ok := p.resolve(focus); ok {
			_, top, bottom := flow.Caret(min(max(s.text, flow.Row.Start), flow.Row.End), focus.Affinity)
			y += float64(float32(offset)+top) / float64(p.pxPerDp)
			end = y + float64(bottom-top)/float64(p.pxPerDp)
		}
	}
	top := p.vp.Offset()
	height := float64(gtx.Constraints.Max.Y) / float64(p.pxPerDp)
	switch {
	case y < top:
		p.vp.ScrollBy(y - top)
	case end > top+height:
		p.vp.ScrollBy(end - top - height)
	}
	p.pinTop = false
	p.captureAnchor()
}

func (p *pane) moveCaret(forward, word bool) {
	if p.scope.Len() == 0 {
		return
	}
	if !p.sel.Selected {
		edge := p.scope.Edge(0, false)
		if !forward {
			edge = p.scope.Edge(p.scope.Len()-1, true)
		}
		p.sel.Selection, p.sel.Selected = document.Caret(edge), true
	} else {
		p.extend(forward, word)
		p.sel.Selection = document.Caret(p.sel.Selection.Focus)
	}
	p.goalSet, p.revealCaret = false, true
}

func (p *pane) extendLine(down bool, flow document.Layout) {
	focus := p.sel.Selection.Focus
	if !p.goalSet {
		x, err := p.vp.CaretX(p.scope, focus, flow)
		if err != nil {
			return
		}
		p.goal, p.goalSet = x, true
	}
	move := p.vp.LineUp
	if down {
		move = p.vp.LineDown
	}
	if next, moved, err := move(p.scope, focus, p.goal, flow); err == nil && moved {
		p.sel.Selection = p.sel.Selection.Extend(next)
	}
}

func (p *pane) extendEdge(end, whole bool, flow document.Layout) {
	focus := p.sel.Selection.Focus
	var next document.Position
	var err error
	switch {
	case whole && end:
		next = p.scope.Edge(p.scope.Len()-1, true)
	case whole:
		next = p.scope.Edge(0, false)
	case end:
		next, err = p.vp.LineEnd(p.scope, focus, flow)
	default:
		next, err = p.vp.LineStart(p.scope, focus, flow)
	}
	if err == nil {
		p.sel.Selection = p.sel.Selection.Extend(next)
	}
}

func (p *pane) extend(forward, word bool) {
	if !p.sel.Selected {
		return
	}
	focus := p.sel.Selection.Focus
	e, ok := p.scope.Index(focus.Document)
	if !ok {
		return
	}
	doc, _ := p.scope.Loaded(e)
	step := document.StepGrapheme
	if word {
		step = document.StepWord
	}
	move := doc.Previous
	if forward {
		move = doc.Next
	}
	next, moved, err := move(focus, step)
	if err != nil {
		return
	}
	if !moved {
		next, moved = p.crossDocument(e, forward)
	}
	if moved {
		p.sel.Selection = p.sel.Selection.Extend(next)
	}
}

func (p *pane) crossDocument(e int, forward bool) (document.Position, bool) {
	step := -1
	if forward {
		step = 1
	}
	for at := e + step; at >= 0 && at < p.scope.Len(); at += step {
		doc, ok := p.scope.Loaded(at)
		if !ok || doc.Len() == 0 {
			continue
		}
		if forward {
			start, err := doc.Start()
			return start, err == nil
		}
		end, err := doc.End()
		return end, err == nil
	}
	return document.Position{}, false
}

func (t *Transcript) pointerFor(gtx layout.Context, p *pane, tag event.Tag) {
	filter := pointer.Filter{
		Target: tag, Kinds: pointer.Press | pointer.Drag | pointer.Release | pointer.Scroll,
		ScrollX: pointer.ScrollRange{Min: -1 << 24, Max: 1 << 24},
		ScrollY: pointer.ScrollRange{Min: -1 << 24, Max: 1 << 24},
	}
	for {
		ev, ok := gtx.Event(filter)
		if !ok {
			return
		}
		if e, ok := ev.(pointer.Event); ok {
			t.handlePointer(gtx, p, tag, e)
		}
	}
}

func (t *Transcript) handlePointer(gtx layout.Context, p *pane, tag event.Tag, e pointer.Event) {
	switch e.Kind {
	case pointer.Scroll:
		t.scrollPointer(p, e)
	case pointer.Press:
		gtx.Execute(key.FocusCmd{Tag: tag})
		t.press(gtx, p, e)
	case pointer.Drag:
		t.drag(gtx, p, e)
	case pointer.Release:
		t.release(p, e)
	case pointer.Cancel:
		p.press = pressState{}
	case pointer.Move, pointer.Enter, pointer.Leave:
	}
}

func (t *Transcript) scrollPointer(p *pane, e pointer.Event) {
	if e.Scroll.X != 0 || e.Modifiers.Contain(key.ModShift) {
		pl, local, ok := p.placedAt(e.Position)
		if ok && pl.split != nil {
			side := 0
			if local.X >= float32(pl.split.width/2) {
				side = 1
			}
			dx := e.Scroll.X
			if dx == 0 {
				dx = e.Scroll.Y
			}
			pl.row.diffActions.scroll(side, dx, float32(pl.split.cells[side].width))
			return
		}
	}
	p.scrolled += float64(e.Scroll.Y / p.pxPerDp)
}

func (p *pane) clicks(now time.Time, at f32.Point) int {
	near := at.Sub(p.press.where)
	if now.Sub(p.press.at) < multiClick && near.X*near.X+near.Y*near.Y < 64 {
		return p.press.count + 1
	}
	return 1
}

func (t *Transcript) press(gtx layout.Context, p *pane, e pointer.Event) {
	pos, ok := p.hit(e.Position)
	p.press = pressState{at: gtx.Now, where: e.Position, count: p.clicks(gtx.Now, e.Position), drag: ok}
	p.goalSet = false
	if !ok {
		p.sel.Selected = false
		return
	}
	if p.press.count == 1 && e.Modifiers == 0 {
		p.press.link, p.press.linked = p.linkAt(e.Position)
	}
	switch {
	case e.Modifiers.Contain(key.ModShift) && p.sel.Selected:
		p.sel.Selection = p.sel.Selection.Extend(pos)
	case p.press.count == 2:
		p.selectWith(pos, (*document.Document).Word)
	case p.press.count >= 3:
		p.selectLine(e.Position)
	default:
		p.sel.Selection, p.sel.Selected = document.Caret(pos), true
	}
}

func (p *pane) selectWith(pos document.Position, pick func(*document.Document, document.Position) (document.Selection, error)) {
	e, ok := p.scope.Index(pos.Document)
	if !ok {
		return
	}
	doc, _ := p.scope.Loaded(e)
	if sel, err := pick(doc, pos); err == nil {
		p.sel.Selection, p.sel.Selected = sel, true
	}
}

func (t *Transcript) drag(gtx layout.Context, p *pane, e pointer.Event) {
	if !p.press.drag || !p.sel.Selected {
		return
	}
	p.press.linked = false
	p.press.dragAt = e.Position
	if e.Position.Y < 0 || e.Position.Y >= float32(gtx.Constraints.Max.Y) {
		if p.press.dragTime.IsZero() {
			p.press.dragTime = gtx.Now
		}
		return
	}
	p.press.dragTime = time.Time{}
	if pos, ok := p.hit(e.Position); ok {
		p.sel.Selection = p.sel.Selection.Extend(pos)
	}
}

func (t *Transcript) scrollDrag(gtx layout.Context, p *pane) {
	if !p.press.drag || p.press.dragTime.IsZero() {
		return
	}
	elapsed := min(max(gtx.Now.Sub(p.press.dragTime), 0), busyPoll)
	p.press.dragTime = gtx.Now
	p.vp.DragScroll(float64(p.press.dragAt.Y/p.pxPerDp), elapsed.Seconds())
	p.pinTop = false
	p.captureAnchor()
	gtx.Execute(op.InvalidateCmd{At: gtx.Now.Add(dragFrame)})
}

func (t *Transcript) selectDragEdge(gtx layout.Context, p *pane) {
	if !p.press.drag || p.press.dragTime.IsZero() {
		return
	}
	at := p.press.dragAt
	after := at.Y >= 0
	if after {
		at.Y = float32(gtx.Constraints.Max.Y - 1)
	} else {
		at.Y = 0
	}
	if pos, ok := p.hit(at); ok {
		p.sel.Selection = p.sel.Selection.Extend(pos)
		return
	}
	nearest := p.dragEdgeRow(int(at.Y), after)
	if nearest == nil {
		return
	}
	top := float32(nearest.y + nearest.text.Y)
	local := f32.Pt(at.X-float32(nearest.text.X), min(max(at.Y-top, 0), float32(nearest.size().Y-1)))
	if pos, ok := nearest.hit(local); ok {
		p.sel.Selection = p.sel.Selection.Extend(pos)
	}
}

func (p *pane) dragEdgeRow(y int, bottom bool) *placed {
	var nearest *placed
	for i := range p.placed {
		pl := &p.placed[i]
		height := pl.size().Y
		if height == 0 {
			continue
		}
		top := pl.y + pl.text.Y
		if bottom && top <= y {
			nearest = pl
		}
		if !bottom && top+height > y {
			return pl
		}
	}
	return nearest
}

func (p *pane) placedAt(at f32.Point) (placed, f32.Point, bool) {
	for _, pl := range p.placed {
		top := float32(pl.y + pl.text.Y)
		if at.Y < top || at.Y >= top+float32(pl.size().Y) {
			continue
		}
		return pl, f32.Pt(at.X-float32(pl.text.X), at.Y-top), true
	}
	return placed{}, f32.Point{}, false
}

func (p *pane) hit(at f32.Point) (document.Position, bool) {
	pl, local, ok := p.placedAt(at)
	if !ok {
		return document.Position{}, false
	}
	return pl.hit(local)
}

func (p *pane) selectLine(at f32.Point) {
	pl, local, ok := p.placedAt(at)
	if !ok {
		return
	}
	if sel, err := pl.row.doc.LineSelection(pl.block, pl.flow(), local.Y); err == nil {
		p.sel.Selection, p.sel.Selected = sel, true
	}
}

func (t *Transcript) openLatest(gtx layout.Context, p *pane) {
	if t.review.active() {
		return
	}
	for i := len(p.order) - 1; i >= 0; i-- {
		r := p.order[i]
		if r.synth == synthNone && !r.entry.Live() {
			t.openReview(gtx, r, &t.focus)
			return
		}
	}
}

func itoa(n int) string {
	return strconv.Itoa(n)
}

func (p *pane) seekDiff() {
	jump := p.diffJump
	p.diffJump = nil
	if jump == nil || jump.row.doc == nil {
		return
	}
	r := jump.row
	point := jump.point
	at := r.diffOffset(point)
	if at < 0 {
		return
	}
	pos, err := r.doc.Position(at, document.After)
	if err != nil {
		return
	}
	if point.hunk != 0 {
		p.unfoldDiff(r, pos)
	}
	if row, ok := p.vp.Locate(p.scope, pos); ok {
		p.vp.ScrollBy(p.vp.RowTop(row) - p.vp.Offset())
		p.pinTop = row == 0
		p.captureAnchor()
	}
}

func (r *row) diffOffset(point diffPoint) int {
	if point.hunk == 0 && point.file >= 0 && point.file < len(r.sheet.diffFiles) {
		return r.sheet.diffLines[r.sheet.diffFiles[point.file]].start
	}
	for _, line := range r.sheet.diffHunks {
		d := r.sheet.diffLines[line]
		if d.file == point.file && d.hunk == point.hunk {
			return d.start
		}
	}
	return -1
}

func (p *pane) unfoldDiff(r *row, pos document.Position) {
	s, ok := p.resolve(pos)
	if !ok {
		return
	}
	key := r.doc.Block(s.block).Key
	for _, fold := range r.doc.Folds() {
		if fold.Key == key && p.scope.Folded(key) {
			p.sel.ToggleFold(p.vp, fold)
			p.vp.Sync(p.scope, p.estimate)
			clear(p.accessible)
			p.accessible = p.accessible[:0]
			return
		}
	}
}
