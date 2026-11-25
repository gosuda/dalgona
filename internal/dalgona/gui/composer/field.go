//go:build gui

package composer

import (
	"errors"

	"gioui.org/io/key"
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const pasteKey key.Name = "V"

func (d *Dock) handleEditor(gtx layout.Context, env frame) {
	for {
		ev, ok := d.editor.Update(gtx)
		if !ok {
			return
		}
		d.handleEvent(gtx, env, ev)
	}
}

func (d *Dock) handleEvent(gtx layout.Context, env frame, ev platform.EditorEvent) {
	switch ev.Kind {
	case platform.EditorSubmit:
		d.observeText(env)
		d.enter(gtx, env, ev.Modifiers)
	case platform.EditorCancel:
		d.escape(gtx, env)
	case platform.EditorKey:
		d.observeText(env)
		d.claimedKey(gtx, env, ev)
	case platform.EditorRejected:
		d.notice.reject(rejectionText(ev.Err), d.editor.TakeRejected())
	case platform.EditorChange:
	}
}

func (d *Dock) enter(gtx layout.Context, env frame, mods key.Modifiers) {
	if !d.listTakesEnter(env) {
		d.act(env, mods.Contain(key.ModAlt))
	}
}

func (d *Dock) act(env frame, alt bool) {
	e := parseEntry(d.editor.Draft().Text)
	switch e.kind {
	case entryShell:
		d.runShell(env, e, alt)
	case entryCommand:
		d.submitCommand(env, e)
	case entryMessage:
		d.submitMessage(env, e.body, alt && running(env))
	}
}

func (d *Dock) listTakesEnter(env frame) bool {
	switch {
	case d.palette.open(d):
		d.openSelected(env)
	case d.files.open(d):
		d.files.insert(d)
	case d.files.pending(env):
	default:
		return false
	}
	return true
}

func (d *Dock) escape(gtx layout.Context, env frame) {
	switch {
	case d.palette.open(d):
		d.palette.closed = true
	case d.files.open(d) || d.files.pending(env):
		d.files.dismiss()
	case running(env):
		if d.guard.escape(gtx.Now, rateLimited(env)) {
			d.interrupt(env)
		}
	}
}

func (d *Dock) interrupt(env frame) {
	d.guard = interruptGuard{}
	env.Submit(state.Interrupt{Window: env.Window, Thread: env.Thread})
}

func (d *Dock) claimedKey(gtx layout.Context, env frame, ev platform.EditorEvent) {
	switch {
	case ev.Name == pasteKey && ev.Modifiers.Contain(key.ModShortcut):
		d.paste(gtx, env, ev)
	case ev.Name == key.NameUpArrow || ev.Name == key.NameDownArrow:
		d.arrow(env, ev.Name == key.NameUpArrow)
	case ev.Name == key.NameTab && ev.Modifiers == 0:
		d.listTakesEnter(env)
	}
}

func (d *Dock) paste(gtx layout.Context, env frame, ev platform.EditorEvent) {
	if ev.Modifiers.Contain(key.ModShift) {
		d.attach.paste(gtx, d, env, platform.PasteImage)
		return
	}
	d.attach.paste(gtx, d, env, platform.PasteOrdinary)
}

func (d *Dock) arrow(env frame, up bool) {
	step := 1
	if up {
		step = -1
	}
	switch {
	case d.palette.open(d):
		d.palette.move(step)
	case d.files.open(d):
		d.files.move(step)
	case d.files.pending(env):
	case up:
		d.recallOlder(env)
	default:
		d.recallNewer(env)
	}
}

func (d *Dock) history(env frame) []string {
	var prompts []string
	if env.Thread != (state.ThreadKey{}) {
		prompts = env.view.Prompts(env.Thread)
	}
	return recallHistory(prompts, d.sent[env.Thread])
}

func (d *Dock) recallOlder(env frame) {
	current := d.editor.Draft()
	if current.Selection != (textinput.Range{}) {
		return
	}
	if next, ok := d.recall.older(d.history(env), current); ok {
		d.showRecall(next)
	}
}

func (d *Dock) recallNewer(env frame) {
	current := d.editor.Draft()
	if current.Selection.Start != current.Selection.End || d.recall.idle() {
		return
	}
	if next, ok := d.recall.newer(d.history(env)); ok {
		d.showRecall(next)
	}
}

func (d *Dock) showRecall(next textinput.Draft) {
	if d.setEditor(next) == nil {
		d.palette.reopen()
	}
}

func (d *Dock) claim() {
	names := []key.Name{pasteKey}
	if d.palette.open(d) || d.files.open(d) || d.files.requested() {
		d.editor.Claim(append(names, key.NameUpArrow, key.NameDownArrow, key.NameTab)...)
		return
	}
	current := d.editor.Draft()
	if current.Selection.Start == current.Selection.End {
		if current.Selection.Start == 0 {
			names = append(names, key.NameUpArrow)
		}
		if current.Selection.End == len(current.Text) || !d.recall.idle() {
			names = append(names, key.NameDownArrow)
		}
	}
	d.editor.Claim(names...)
}

func rejectionText(err error) string {
	switch {
	case errors.Is(err, textinput.TextTooLarge):
		return "The text exceeds 15 MiB; shorten it or attach less."
	case errors.Is(err, textinput.DisallowedControl):
		return "The text contains a NUL character, which a message cannot carry; remove it."
	case errors.Is(err, textinput.InvalidEncoding):
		return "The text is not valid UTF-8; copy it again from its source."
	case errors.Is(err, platform.ErrComposing):
		return "Finish or cancel the text composition first."
	}
	return err.Error()
}
