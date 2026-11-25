//go:build gui

package composer

import (
	"time"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type keywordRequest struct {
	text      string
	due       time.Time
	submitted string
}

func (k *keywordRequest) advance(gtx layout.Context, d *Dock, env frame) {
	text := d.editor.Draft().Text
	if env.Thread == (state.ThreadKey{}) || whitespaceOnly(text) || len(text) > keywordMatchLimit {
		k.text, k.due = text, time.Time{}
		return
	}
	if text != k.text {
		k.text, k.due = text, gtx.Now.Add(settleDelay)
	}
	if k.text == k.submitted || k.due.IsZero() {
		return
	}
	if gtx.Now.Before(k.due) {
		gtx.Execute(op.InvalidateCmd{At: k.due})
		return
	}
	k.submitted = k.text
	env.Submit(state.MatchKeywords{Key: d.key, Text: k.text})
}

func (d *Dock) keywordLine(env frame) string {
	text := d.editor.Draft().Text
	kw := env.view.DraftKeywords(d.key)
	if kw.Text != text || text == "" {
		return ""
	}
	spans, ok := KeywordSpans(text, kw.Matches)
	if !ok {
		return ""
	}
	return KeywordWords(spans)
}

type shellChipKey struct {
	key    state.DraftKey
	ticket uint64
}

type shellState struct {
	ticket  uint64
	textGen uint64
	chipped map[shellChipKey]bool
	shown   map[uint64]bool
	rows    shellRows
}

func (s *shellState) start(d *Dock, env frame, command string) {
	s.ticket, s.textGen = d.nextTicket(), d.draft.textGen
	env.Submit(state.RunShell{Key: d.key, Command: command, Ticket: s.ticket})
}

func (s *shellState) run(d *Dock, env frame, ticket uint64) (state.ShellRun, bool) {
	for _, r := range env.view.ShellRuns(d.key) {
		if r.Ticket == ticket {
			return r, true
		}
	}
	return state.ShellRun{}, false
}

func (s *shellState) waiting(d *Dock, env frame) bool {
	if s.ticket == 0 {
		return false
	}
	r, ok := s.run(d, env, s.ticket)
	return !ok || r.Phase == state.ShellAuthorizing
}

func (s *shellState) advance(d *Dock, env frame) {
	s.settleField(d, env)
	for _, r := range env.view.ShellRuns(d.key) {
		s.chip(d, env, r)
	}
}

func (s *shellState) settleField(d *Dock, env frame) {
	if s.ticket == 0 {
		return
	}
	r, ok := s.run(d, env, s.ticket)
	if !ok || r.Phase == state.ShellAuthorizing {
		return
	}
	s.ticket = 0
	if r.Phase == state.ShellDeclined {
		d.notice.fail("Terminal permission was declined; the command did not run.")
		return
	}
	if r.Phase == state.ShellFailed {
		d.notice.fail(shellFailure(r))
		return
	}
	if d.draft.textGen == s.textGen {
		d.clearField(env)
	}
}

func (s *shellState) chip(d *Dock, env frame, r state.ShellRun) {
	id := shellChipKey{key: d.key, ticket: r.Ticket}
	if r.Phase != state.ShellExited || r.ExitCode == nil || s.chipped[id] {
		return
	}
	s.chipped[id] = true
	output, lines := shellOutput(r.Output)
	d.draft.chips = append(d.draft.chips, state.Attachment{
		ID:   d.nextChipID("shell"),
		Kind: state.AttachmentText,
		Name: shellChipName(r.N, *r.ExitCode, lines, r.Truncated),
		Text: shellText(r.Command, *r.ExitCode, output, r.Truncated),
	})
	d.push(env)
}

func shellFailure(r state.ShellRun) string {
	if r.Err == nil {
		return "The shell command failed before it ran."
	}
	return "The shell command failed: " + r.Err.Error()
}
