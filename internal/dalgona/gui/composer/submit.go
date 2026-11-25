//go:build gui

package composer

import (
	"fmt"
	"slices"
	"strings"
	"unicode"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	followUpLimit   = 32
	followUpBytes   = 256 << 10
	shellTextBytes  = 64 << 10
	noticeEmpty     = "Enter a message before sending."
	noticeNoThread  = "Open a thread or start one with /new before sending."
	noticeQueueFull = "The follow-up queue is full (32 messages or 256 KiB)"
	noticeShellAlt  = "Shell commands run with Enter"
	noticeShellNone = "Open a thread before running a shell command"
	noticeShellText = "Enter a command after !."
	noticeShellSize = "The command exceeds 64 KiB; shorten it."
	noticeShellWait = "The previous command is waiting for terminal permission"
	noticeSpace     = "A command takes no text after its name. Start with // to send a message that begins with /."
)

type submitKind uint8

const (
	submitSend submitKind = iota
	submitSteer
	submitQueue
)

func unknownCommand(query string) string {
	return fmt.Sprintf("No command named /%s. Start with // to send a message that begins with /.", query)
}

func whitespaceOnly(s string) bool {
	return strings.IndexFunc(s, func(r rune) bool { return !unicode.IsSpace(r) }) < 0
}

func sendBlock(env frame) string {
	tv, ok := threadOf(env)
	switch {
	case env.Thread == (state.ThreadKey{}):
		return noticeNoThread
	case !ok:
		return fmt.Sprintf("The thread is still opening on %s; wait for it to load.", env.Thread.Endpoint)
	case tv.Missing:
		return fmt.Sprintf("This thread no longer exists on %s; open another thread.", env.Thread.Endpoint)
	}
	switch tv.Phase {
	case client.PhaseOffline, client.PhaseIncompatible, client.PhaseFailed:
		return fmt.Sprintf("%s is %s; reconnect before sending", env.Thread.Endpoint, tv.Phase)
	case client.PhaseIdle, client.PhaseConnecting, client.PhaseConnected, client.PhaseReconnecting:
	}
	return ""
}

func imagesRefused(env frame, chips []state.Attachment) bool {
	if !hasImages(chips) || env.Thread == (state.ThreadKey{}) {
		return false
	}
	accepts, known := env.view.AcceptsImages(env.Thread)
	return known && !accepts
}

func (d *Dock) submitMessage(env frame, body string, queue bool) {
	if reason := sendBlock(env); reason != "" {
		d.notice.fail(reason)
		return
	}
	kind := submitSend
	switch {
	case queue:
		kind = submitQueue
	case running(env):
		kind = submitSteer
	}
	typed := body
	if whitespaceOnly(body) {
		typed = ""
	}
	if refusal := d.refuseMessage(env, kind, typed); refusal != "" {
		d.notice.fail(refusal)
		return
	}
	d.commit(env, kind, typed)
}

func (d *Dock) refuseMessage(env frame, kind submitKind, typed string) string {
	carried := d.carried(kind)
	switch {
	case typed == "" && len(carried) == 0:
		return noticeEmpty
	case imagesRefused(env, carried):
		return string(errImagesRefused)
	case inputBytes(typed, carried) > inputBudgetBytes:
		return string(errInputBudget)
	case kind == submitQueue && queueFull(env.view.FollowUps(d.key), typed):
		return noticeQueueFull
	}
	return ""
}

func (d *Dock) carried(kind submitKind) []state.Attachment {
	if kind == submitSend {
		return d.draft.chips
	}
	return nil
}

func queueFull(queue []state.FollowUp, typed string) bool {
	used := len(typed)
	for _, f := range queue {
		used += len(f.Text)
		for _, a := range f.Attachments {
			used += len(a.Text)
		}
	}
	return len(queue) >= followUpLimit || used > followUpBytes
}

func (d *Dock) commit(env frame, kind submitKind, typed string) {
	current := d.editor.Draft()
	d.push(env)
	payload := state.Draft{Text: typed, Attachments: slices.Clone(d.carried(kind)), Revision: d.draft.revision}
	switch kind {
	case submitSend:
		env.Submit(state.Send{Key: d.key, Draft: payload})
	case submitSteer:
		env.Submit(state.Steer{Key: d.key, Draft: payload})
	case submitQueue:
		env.Submit(state.Enqueue{Key: d.key, Draft: payload})
	}
	if kind != submitQueue && typed != "" {
		d.sent[env.Thread] = append(d.sent[env.Thread], typed)
	}
	d.pending = pendingSubmit{revision: d.draft.revision, draft: current, chips: slices.Clone(d.draft.chips)}
	if kind == submitSend {
		d.releaseChips()
		d.draft.chips = nil
	}
	if d.setEditor(textinput.Draft{}) == nil {
		d.recall.reset()
		d.notice.clear()
	}
}

func (d *Dock) submitCommand(env frame, e entry) {
	query, ok := e.commandQuery()
	if !ok {
		d.notice.fail(noticeSpace)
		return
	}
	c, found := lookupCommand(query)
	if !found {
		d.notice.fail(unknownCommand(query))
		return
	}
	d.openCommand(env, c)
}

func (d *Dock) openCommand(env frame, c command) {
	if reason := c.unmet(d.context(env)); reason != "" {
		d.notice.fail(reason)
		return
	}
	d.clearField(env)
	d.notice.clear()
	route := shell.Route{Panel: c.name, Thread: env.Thread}
	if c.name == "help" {
		route.Context = "composer"
	}
	env.Open(route)
}

func (d *Dock) runShell(env frame, e entry, alt bool) {
	if refusal := d.refuseShell(env, e, alt); refusal != "" {
		d.notice.fail(refusal)
		return
	}
	d.shell.start(d, env, e.body)
}

func (d *Dock) refuseShell(env frame, e entry, alt bool) string {
	tv, ok := threadOf(env)
	switch {
	case alt:
		return noticeShellAlt
	case !ok || tv.Thread.Cwd == "":
		return noticeShellNone
	case whitespaceOnly(e.body):
		return noticeShellText
	case len(e.body) > shellTextBytes:
		return noticeShellSize
	case d.shell.waiting(d, env):
		return noticeShellWait
	}
	return ""
}
