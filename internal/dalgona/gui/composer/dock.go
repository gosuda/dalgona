//go:build gui

package composer

import (
	"context"
	"slices"
	"strconv"

	"gioui.org/io/event"
	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const arrivalQueue = 8

type Dock struct {
	host     Host
	editor   *platform.Editor
	key      state.DraftKey
	bound    bool
	draft    localDraft
	pending  pendingSubmit
	ticket   uint64
	sent     map[state.ThreadKey][]string
	recall   recall
	guard    interruptGuard
	palette  palette
	files    completion
	keywords keywordRequest
	shell    shellState
	notice   fieldNotice
	attach   attachments
	queue    followUpQueue
	ui       dockWidgets
}

type localDraft struct {
	chips    []state.Attachment
	revision uint64
	text     string
	textGen  uint64
}

type pendingSubmit struct {
	revision uint64
	draft    textinput.Draft
	chips    []state.Attachment
}

func New(host Host) (*Dock, error) {
	ed, err := platform.NewEditor(messagePolicy, textinput.Draft{})
	if err != nil {
		return nil, err
	}
	pasteCtx, stopPaste := context.WithCancel(context.Background())
	d := &Dock{
		host:   host,
		editor: ed,
		sent:   map[state.ThreadKey][]string{},
		recall: recall{browsing: -1},
		shell:  shellState{chipped: map[shellChipKey]bool{}, shown: map[uint64]bool{}},
		attach: attachments{arrivals: make(chan arrival, arrivalQueue), ctx: pasteCtx, stop: stopPaste, shown: map[string]bool{}, images: map[string]*chipImage{}},
		queue:  followUpQueue{edit: map[uint64]*clickable{}, remove: map[uint64]*clickable{}},
	}
	return d, nil
}

func (d *Dock) FocusTag() event.Tag { return d.editor.Tag() }

func (d *Dock) Close() {
	if d.attach.stop != nil {
		d.attach.stop()
	}
	d.releaseChips()
}

type stateView interface {
	Draft(key state.DraftKey) state.Draft
	FollowUps(key state.DraftKey) []state.FollowUp
	Thread(key state.ThreadKey) (*state.ThreadView, bool)
	DraftKeywords(key state.DraftKey) state.DraftKeywords
	FileSearch(key state.DraftKey) state.FileSearch
	ShellRuns(key state.DraftKey) []state.ShellRun
	AcceptsImages(key state.ThreadKey) (accepts, known bool)
	Prompts(key state.ThreadKey) []string
}

type frame struct {
	shell.Env
	view stateView
}

func (d *Dock) Layout(gtx layout.Context, env shell.Env) layout.Dimensions {
	return d.render(gtx, frame{Env: env, view: env.View})
}

func (d *Dock) render(gtx layout.Context, env frame) layout.Dimensions {
	d.bind(env)
	d.adopt(env)
	d.attach.drain(gtx, d, env)
	d.handleClicks(gtx, env)
	d.handleEditor(gtx, env)
	d.observeText(env)
	d.files.advance(gtx, d, env)
	d.keywords.advance(gtx, d, env)
	d.shell.advance(d, env)
	d.claim()
	if d.guard.armed(gtx.Now) {
		gtx.Execute(op.InvalidateCmd{At: d.guard.until})
	}
	return d.layout(gtx, env)
}

func draftKey(env frame) state.DraftKey {
	return state.DraftKey{Endpoint: env.Thread.Endpoint, Window: env.Window, Thread: env.Thread.Thread}
}

func (d *Dock) bind(env frame) {
	key := draftKey(env)
	if d.bound && key == d.key {
		return
	}
	d.bound, d.key = true, key
	d.guard = interruptGuard{}
	d.notice = fieldNotice{}
	d.files.reset()
	d.keywords = keywordRequest{}
	d.load(env.view.Draft(key))
}

func (d *Dock) load(v state.Draft) {
	end := len(v.Text)
	if d.setEditor(textinput.Draft{Text: v.Text, Selection: textinput.Range{Start: end, End: end}}) != nil {
		return
	}
	d.releaseChips()
	d.draft.chips = slices.Clone(v.Attachments)
	d.draft.revision = v.Revision
	d.pending = pendingSubmit{}
	d.recall.reset()
	d.palette.reopen()
}

func (d *Dock) setEditor(next textinput.Draft) error {
	if err := d.editor.SetDraft(next); err != nil {
		d.notice.fail(rejectionText(err))
		return err
	}
	d.noteText(next.Text)
	return nil
}

func (d *Dock) noteText(text string) {
	if text == d.draft.text {
		return
	}
	d.draft.text = text
	d.draft.textGen++
}

func (d *Dock) adopt(env frame) {
	v := env.view.Draft(d.key)
	switch {
	case v.Revision > d.draft.revision && !d.editor.Composing():
		d.load(v)
	case d.pending.revision != 0 && v.Revision >= d.pending.revision:
		d.settle(env, v)
	}
}

func (d *Dock) settle(env frame, v state.Draft) {
	p := d.pending
	d.pending = pendingSubmit{}
	if d.refused(p, v) && d.setEditor(p.draft) == nil {
		d.draft.chips = p.chips
	}
	if v.Text != d.draft.text || !slices.Equal(v.Attachments, d.draft.chips) {
		d.push(env)
	}
}

func (d *Dock) refused(p pendingSubmit, v state.Draft) bool {
	kept := v.Text != "" || len(v.Attachments) > 0
	return kept && v.Revision == p.revision && d.draft.revision == p.revision && !d.editor.Composing()
}

func (d *Dock) push(env frame) {
	d.draft.revision++
	env.Submit(state.PutDraft{Key: d.key, Draft: d.stored()})
}

func (d *Dock) stored() state.Draft {
	return state.Draft{Text: d.editor.Draft().Text, Attachments: slices.Clone(d.draft.chips), Revision: d.draft.revision}
}

func (d *Dock) nextTicket() uint64 {
	d.ticket++
	return d.ticket
}

func (d *Dock) nextChipID(kind string) string {
	for {
		id := kind + "-" + strconv.FormatUint(d.nextTicket(), 10)
		if !slices.ContainsFunc(d.draft.chips, func(c state.Attachment) bool { return c.ID == id }) {
			return id
		}
	}
}

func (d *Dock) clearField(env frame) {
	if d.setEditor(textinput.Draft{}) == nil {
		d.push(env)
	}
	d.recall.reset()
}

func (d *Dock) observeText(env frame) {
	text := d.editor.Draft().Text
	if text == d.draft.text {
		return
	}
	d.noteText(text)
	d.recall.reset()
	d.palette.reopen()
	d.notice.clear()
	d.push(env)
}

func threadOf(env frame) (*state.ThreadView, bool) {
	if env.Thread == (state.ThreadKey{}) {
		return nil, false
	}
	return env.view.Thread(env.Thread)
}

func running(env frame) bool {
	tv, ok := threadOf(env)
	return ok && tv.ActiveTurn != ""
}

func rateLimited(env frame) bool {
	tv, ok := threadOf(env)
	if !ok || tv.ActiveTurn == "" {
		return false
	}
	at := slices.IndexFunc(tv.Turns, func(t state.TurnView) bool { return t.ID == tv.ActiveTurn })
	return at >= 0 && tv.Turns[at].RetryError != nil
}

func (d *Dock) context(env frame) threadContext {
	return threadContext{open: env.Thread != (state.ThreadKey{}), running: running(env)}
}
