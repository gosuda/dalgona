//go:build gui

package transcript

import (
	"errors"
	"fmt"
	"maps"
	"slices"
	"strconv"
	"time"
	"unicode/utf8"
	"unsafe"
	"weak"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	accessPageBytes    = platform.TextObjectLimit / 4
	accessPageLimit    = 128
	accessSourceFanout = 64
	accessDeadline     = 30 * time.Second
)

var errAccessTimeout = errors.New("the content request timed out")

type accessRange struct {
	document document.Identity
	revision string
	content  string
	start    uint64
	end      uint64
}

type accessNavigation struct {
	key    accessRange
	action platform.AccessibilityAction
}

type accessPage struct {
	value    string
	err      error
	deadline time.Time
	used     uint64
	view     weak.Pointer[state.ThreadView]
	seen     uint64
}

func (t *Transcript) registerContent(gtx layout.Context, p *pane, index int, r *row) {
	for _, b := range r.sheet.sources {
		if b.serveIncompleteLargeOrFull(r.sheet.full) {
			t.registerSource(gtx, p, index, r, b)
		}
	}
}

func (t *Transcript) registerSource(gtx layout.Context, p *pane, index int, r *row, b *body) {
	id := platform.NodeID(access.Path(r.id.Turn, r.id.Item, r.id.Draft, "content", b.key.content))
	base := accessRange{document: r.id, revision: r.entry.Item.Header.ItemRevision, content: b.key.content, end: b.total}
	target := sourceTarget{pane: p, row: index, id: id, body: b, revision: base.revision}
	width := uint64(accessPageBytes)
	for (b.total-1)/width >= accessSourceFanout {
		width *= accessSourceFanout
	}
	n := platform.SemanticNode{ID: id, Role: platform.RoleDocument, Name: "Full source: " + b.ref.Field, TextContainer: width == accessPageBytes}
	access.Offscreen(gtx, n, access.Actions{})
	t.registerSourceChildren(gtx, target, base, width)
}

func (t *Transcript) registerSourceChildren(gtx layout.Context, target sourceTarget, parent accessRange, width uint64) {
	for start := parent.start; start < parent.end; {
		k := parent
		k.start, k.end = start, start+min(parent.end-start, width)
		if parent.end-k.end < utf8.UTFMax {
			k.end = parent.end
		}
		if width == accessPageBytes {
			t.registerSourceRange(gtx, target, k)
		} else {
			t.registerSourceGroup(gtx, target, k, width)
		}
		start = k.end
	}
}

func (t *Transcript) registerSourceGroup(gtx layout.Context, target sourceTarget, k accessRange, width uint64) {
	id := target.id + "/group/" + platform.NodeID(strconv.FormatUint(k.start, 10))
	p := target.pane
	open := k.contains(p.accessGroup)
	n := platform.SemanticNode{ID: id, Role: platform.RoleButton, Name: fmt.Sprintf("Source ranges from byte %d to %d", k.start+1, k.end), Expandable: true, Expanded: open, Action: "press"}
	n.TextContainer = open && width/accessSourceFanout == accessPageBytes
	access.Offscreen(gtx, n, access.Within(target.id, access.Actions{Activate: func() {
		if open {
			p.accessGroup = accessRange{}
		} else {
			p.accessGroup = k
		}
	}}))
	if open {
		target.id = id
		t.registerSourceChildren(gtx, target, k, width/accessSourceFanout)
	}
}

func (k accessRange) contains(other accessRange) bool {
	return other.end > other.start && k.document == other.document && k.revision == other.revision && k.content == other.content && k.start <= other.start && k.end >= other.end
}

func (k accessRange) size() int {
	return int(min(k.end-k.start, accessPageBytes+utf8.UTFMax-1))
}

type sourceTarget struct {
	pane     *pane
	row      int
	id       platform.NodeID
	body     *body
	revision string
}

func (t *Transcript) registerSourceRange(gtx layout.Context, target sourceTarget, k accessRange) {
	p := target.pane
	id := target.id + "/" + platform.NodeID(strconv.FormatUint(k.start, 10))
	n := platform.SemanticNode{ID: id, Role: platform.RoleDocument, Name: fmt.Sprintf("Source bytes %d to %d", k.start+1, k.end), TextState: platform.TextUncached, TextRevision: target.revision, Focusable: true}
	n.TextGroup = access.ID(gtx, string(target.id))
	if page := p.accessPages[k]; page != nil {
		if page.textState() == platform.TextLoading {
			if value, ok := target.body.cachedRange(k); page.inFlight() == 0 && ok && value != "" {
				p.accessReads -= page.inFlight()
				page.value, page.view = value, weak.Pointer[state.ThreadView]{}
			} else {
				t.pollSourceRange(gtx, target, k, page)
			}
		}
		n.TextState, n.Value = page.text()
	}
	a := access.Actions{Tag: &t.focus, Address: func(action platform.AccessibilityAction) bool {
		return target.address(k, n.TextState, action)
	}}
	access.Offscreen(gtx, n, access.Within(target.id, a))
	t.finishSourceRange(gtx, p, n, k)
}

func (t *Transcript) finishSourceRange(gtx layout.Context, p *pane, n platform.SemanticNode, k accessRange) {
	if page := p.accessPages[k]; page != nil {
		page.seen = p.frame
	}
	switch n.TextState {
	case platform.TextReady:
		if p.accessNavigation.key == k {
			access.ResumeAfterPublish(gtx, n.ID, p.accessNavigation.action)
			p.accessNavigation = accessNavigation{}
			gtx.Execute(op.InvalidateCmd{})
		}
	case platform.TextFailed:
		registerSourceError(gtx, p, n.ID, k)
	case platform.TextLoading, platform.TextUncached:
	}
}

func registerSourceError(gtx layout.Context, p *pane, id platform.NodeID, k accessRange) {
	retry := platform.SemanticNode{ID: id + "/retry", Role: platform.RoleButton, TextRevision: k.revision, Name: "Retry loading text", Action: "press"}
	a := access.Actions{Activate: func() { p.accessPages[k] = &accessPage{} }}
	access.Offscreen(gtx, retry, access.Within(id, a))
	notice := platform.SemanticNode{ID: id + "/error", Role: platform.RoleError, Name: "Text could not load: " + p.accessPages[k].err.Error(), Target: access.ID(gtx, string(retry.ID))}
	access.Offscreen(gtx, notice, access.Within(id, access.Actions{}))
}

func (t sourceTarget) address(k accessRange, status platform.TextAvailability, action platform.AccessibilityAction) bool {
	p := t.pane
	if page := p.accessPages[k]; page != nil {
		page.used = p.frame
	}
	navigate := action == platform.ActionFocus || action == platform.ActionScrollIntoView
	if navigate && status == platform.TextReady {
		p.scrolled = p.vp.RowTop(t.row) - p.vp.Offset()
		return true
	}
	if navigate {
		p.accessNavigation = accessNavigation{key: k, action: action}
	}
	if (navigate || action == platform.ActionLoadText) && status == platform.TextUncached {
		p.wantSourceRange(k)
	}
	return false
}

func (p *pane) wantSourceRange(k accessRange) {
	if p.accessPages[k] != nil {
		return
	}
	if len(p.accessPages) >= accessPageLimit {
		p.evictSourceRange()
	}
	if len(p.accessPages) < accessPageLimit {
		if p.accessPages == nil {
			p.accessPages = make(map[accessRange]*accessPage)
		}
		p.accessPages[k] = &accessPage{used: p.frame}
	}
}

func (p *pane) evictSourceRange() {
	var victim accessRange
	var oldest *accessPage
	for k, page := range p.accessPages {
		if k == p.accessNavigation.key || page.textState() == platform.TextLoading {
			continue
		}
		if oldest == nil || page.used < oldest.used {
			victim, oldest = k, page
		}
	}
	if oldest != nil {
		delete(p.accessPages, victim)
	}
}

func (p *pane) pruneSourceRanges() {
	maps.DeleteFunc(p.accessPages, func(_ accessRange, page *accessPage) bool {
		if page.seen == p.frame {
			return false
		}
		p.accessReads -= page.inFlight()
		return true
	})
	if p.accessPages[p.accessNavigation.key] == nil {
		p.accessNavigation = accessNavigation{}
	}
}

func (t *Transcript) pollSourceRange(gtx layout.Context, target sourceTarget, k accessRange, page *accessPage) {
	if page.textState() != platform.TextLoading {
		return
	}
	p, b := target.pane, target.body
	tv, ok := t.env.View.Thread(b.key.thread)
	if !ok {
		return
	}
	key := state.ContentKey{Item: b.key.item, ContentID: k.content, Offset: k.start}
	result, exists := tv.Content(key)
	if exists && !result.Loading && (result.Err == nil || !page.deadline.IsZero() && page.view != weak.Make(tv)) {
		p.accessReads -= page.inFlight()
		page.accept(b, k, result)
		page.view = weak.Pointer[state.ThreadView]{}
		t.env.Submit(state.ReleaseContentPage{Thread: b.key.thread, Key: key})
		return
	}
	t.awaitSourceRange(gtx, p, key, page, tv)
}

func (t *Transcript) awaitSourceRange(gtx layout.Context, p *pane, key state.ContentKey, page *accessPage, tv *state.ThreadView) {
	if page.deadline.IsZero() && p.accessReads < readsInFlight {
		page.deadline, page.view = gtx.Now.Add(accessDeadline), weak.Make(tv)
		p.accessReads++
		t.env.Submit(state.ReadContent{Thread: tv.Key, Item: key.Item, ContentID: key.ContentID, Offset: key.Offset})
	}
	if !page.deadline.IsZero() && !gtx.Now.Before(page.deadline) {
		page.err, page.view = errAccessTimeout, weak.Pointer[state.ThreadView]{}
		p.accessReads--
		return
	}
	gtx.Execute(op.InvalidateCmd{At: gtx.Now.Add(busyPoll)})
}

func (p *accessPage) inFlight() int {
	if p.deadline.IsZero() || p.textState() != platform.TextLoading {
		return 0
	}
	return 1
}

func (p *accessPage) textState() platform.TextAvailability {
	if p.err != nil {
		return platform.TextFailed
	}
	if p.value != "" {
		return platform.TextReady
	}
	return platform.TextLoading
}

func (p *accessPage) text() (platform.TextAvailability, string) {
	return p.textState(), p.value
}

func (p *accessPage) accept(b *body, k accessRange, result state.ContentPage) {
	if result.Err != nil {
		p.err = result.Err
		return
	}
	if result.Total != b.total || uint64(len(result.Data)) < k.required(b.total) {
		p.err = errMismatch
		return
	}
	p.value = sourceText(result.Data, k.size(), k.start != 0)
	if p.value == "" {
		p.err = errNoProgress
	}
}

func (k accessRange) required(total uint64) uint64 {
	return k.end - k.start + min(total-k.end, utf8.UTFMax-1)
}

func (b *body) cachedRange(k accessRange) (string, bool) {
	if k.start < b.start || k.start > b.next || k.required(b.total) > b.next-k.start {
		return "", false
	}
	off := k.start - b.start
	data := b.data[off : off+k.required(b.total)]
	return sourceText(data, k.size(), k.start != 0), true
}

func sourceText(data []byte, end int, leading bool) string {
	start := 0
	for leading && start < min(end, utf8.UTFMax-1) && !utf8.RuneStart(data[start]) {
		start++
	}
	for n := 0; end < len(data) && n < utf8.UTFMax-1 && !utf8.RuneStart(data[end]); n++ {
		end++
	}
	return clean(string(data[start:end]))
}

func (t *Transcript) prepareSheets(p *pane, rows []*row) {
	for _, r := range rows {
		if r.sourceEvicted && !t.sourceWanted(p, r) {
			continue
		}
		r.sourceEvicted = false
		r.sheet = t.sheetFor(p, r)
	}
}

func (t *Transcript) sourceWanted(p *pane, r *row) bool {
	if r.entry.Live() || t.copyNeedsCachedRows(p) {
		return true
	}
	if p.vp.Following() && r.latest {
		return true
	}
	first, last := p.vp.Window()
	for i := first; i < last; i++ {
		entry := p.vp.Row(i).Entry
		if entry < len(p.order) && p.order[entry] == r {
			return true
		}
	}
	return false
}

func (t *Transcript) copyNeedsCachedRows(p *pane) bool {
	return t.pendingCopy != nil && t.pendingCopy.pane == p && (t.review == nil || t.review.spill != p)
}

func (t *Transcript) trimSources(gtx layout.Context, active *pane) {
	l := t.loader
	l.holdMu.Lock()
	defer l.holdMu.Unlock()
	l.dropProjections(l)
	l.presented = t.presentedSourceBytes()
	reviewPane := t.review.sourcePane()
	t.trimInactiveSources(active)
	l.trimPane(active, active.visibleSources())
	l.trimPane(reviewPane, reviewPane.visibleSources())
	l.evict()
	if l.overBudget() {
		t.review.trimFields(l)
	}
	l.trimPane(active, nil)
	l.trimPane(reviewPane, nil)
	l.cachedSource = l.resident + l.presented - l.heldSourceBytes()
	if active != nil && slices.ContainsFunc(active.placed, func(line placed) bool { return line.row.sourceEvicted }) {
		gtx.Execute(op.InvalidateCmd{})
	}
}

func (t *Transcript) releaseWindowSources() {
	l := t.loader
	l.releaseProjections()
	for _, b := range l.bodies {
		b.data = nil
	}
	clear(l.bodies)
	l.wanted, l.resident = nil, 0
	if r := t.review; r != nil {
		r.opened = false
		r.fields, r.pane, r.find, r.spill = nil, nil, nil, nil
		r.tags, r.opener, r.mount = nil, nil, nil
	}
	l.holdMu.Lock()
	l.presented = l.heldSourceBytes()
	l.cachedSource = 0
	l.holdMu.Unlock()
}

func stringBacking(text string) *byte {
	return unsafe.StringData(text) //nolint:gosec // the immutable first-byte address is used only for backing identity.
}

func (t *Transcript) leaseExportSource(d *document.Document) (func(), bool) {
	return t.loader.holdSource(stringBacking(d.Source()), d.ResidentBytes())
}

func (t *Transcript) leaseImageSource(data []byte) (func(), bool) {
	if len(data) == 0 {
		return nil, false
	}
	return t.loader.holdSource(&data[0], cap(data))
}

func (l *loader) holdText(text string) (func(), bool) {
	return l.holdSource(stringBacking(text), len(text))
}

func (l *loader) imageHeld(data []byte) bool {
	return len(data) > 0 && l.holds[&data[0]].owners != 0
}

func (t *Transcript) presentedSourceBytes() int {
	n := t.loader.heldSourceBytes()
	for _, p := range t.panes {
		n += p.sourceBytes(t.loader)
	}
	if p := t.review.sourcePane(); p != nil {
		n += p.sourceBytes(t.loader)
		for _, f := range t.review.fields {
			n += cap(f.data)
			if t.loader.holds[stringBacking(f.suffix)].owners == 0 {
				n += len(f.suffix)
			}
		}
	}
	return n
}

func (t *Transcript) trimInactiveSources(active *pane) {
	for _, p := range t.panes {
		if !t.loader.overBudget() {
			return
		}
		if p != active {
			t.loader.trimPane(p, nil)
		}
	}
}

func (r *reviewer) sourcePane() *pane {
	if !r.active() {
		return nil
	}
	return r.pane
}

func (r *reviewer) trimFields(l *loader) {
	if !r.active() {
		return
	}
	for _, f := range r.fields {
		if !l.overBudget() {
			return
		}
		if cap(f.data) != 0 {
			l.presented -= cap(f.data)
			f.reset()
			f.deferred = true
			r.pane.discardSource(r.pane.rows[f.id], l)
		}
	}
}

func (p *pane) sourceBytes(l *loader) int {
	n := 0
	for _, r := range p.rows {
		r.detachFrameSources()
		if r.folds.doc != r.doc {
			r.folds.doc = nil
		}
		n += r.sourceBytes(l) + r.encodedImageBytes(l.imageHeld)
	}
	for _, page := range p.accessPages {
		n += len(page.value)
	}
	return n
}

func (r *row) sourceBytes(l *loader) int {
	n := len(r.sheet.source)
	if r.doc != nil {
		if r.sheet.source == r.doc.Source() {
			r.sheet.source = r.doc.Source()
			n = 0
		}
		if l.holds[stringBacking(r.doc.Source())].owners == 0 {
			n += r.doc.ResidentBytes()
		}
	}
	for _, path := range r.sheet.diffPaths {
		n += len(path)
	}
	return n
}

func (p *pane) visibleSources() []placed {
	if p == nil {
		return nil
	}
	return p.placed
}

func (l *loader) trimPane(p *pane, protected []placed) {
	if p == nil || !l.overBudget() {
		return
	}
	l.trimEncodedImages(p)
	for _, r := range p.rows {
		if !l.overBudget() {
			return
		}
		if slices.ContainsFunc(protected, func(line placed) bool { return line.row == r }) {
			continue
		}
		p.discardSource(r, l)
	}
	l.trimAccessPages(p)
}

func (l *loader) trimEncodedImages(p *pane) {
	for _, r := range p.rows {
		if !l.overBudget() {
			return
		}
		l.presented -= r.dropEncodedImages(l.imageHeld)
	}
}

func (l *loader) trimAccessPages(p *pane) {
	for k, page := range p.accessPages {
		if !l.overBudget() {
			return
		}
		if k != p.accessNavigation.key && len(page.value) > 0 {
			l.presented -= len(page.value)
			delete(p.accessPages, k)
		}
	}
}

func (p *pane) discardSource(r *row, l *loader) {
	if r == nil {
		return
	}
	n := r.sourceBytes(l)
	if n == 0 {
		return
	}
	l.presented -= n
	if i, ok := p.scope.Index(r.id); ok {
		p.scope.Unload(i)
	}
	r.doc, r.folds.doc = nil, nil
	r.sheet.source, r.sourceEvicted = "", true
	r.sheet.diffLines, r.sheet.diffPaths, r.sheet.diffFiles, r.sheet.diffHunks = nil, nil, nil, nil
	if len(p.placed) > 0 {
		p.placed = slices.DeleteFunc(p.placed, func(line placed) bool { return line.row == r })
	}
	clear(p.accessible)
	p.accessible = p.accessible[:0]
}
