//go:build gui

package decision

import (
	"encoding/json/jsontext"
	"fmt"

	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type gate uint8

const (
	gateNone gate = iota
	gateDetails
	gateChanges
)

type action struct {
	label   string
	key     shortcut
	variant variant
	reply   func(*card) jsontext.Value
	gate    gate
	trigger platform.EditorEventKind
	reason  string
}

type card struct {
	key         state.RequestKey
	params      jsontext.Value
	details     details
	base        []action
	acts        []action
	sent        bool
	sentWarning *state.AnswerWarning
	sentLink    client.Link
	announced   bool
	title       int
	clicks      [4]widget.Clickable
	retry       widget.Clickable
	answers     []answer
	changes     []rpc.FileChange
	diffs       []fileDiff
	loaded      bool
	asked       []rpc.ToolRequestUserInputQuestion
	triggers    []platform.EditorEventKind
	locked      string
	endpoint    string
	serial      uint64
}

func (c *card) refresh(p pending) {
	c.endpoint = p.endpoint
	if c.base == nil || !sameBytes(c.params, p.view.Params) {
		c.params = p.view.Params
		c.details = detailsOf(p.view.Method, p.view.Params)
		c.base = template(c.details)
		c.prepareAnswers()
	}
	if c.sent && (p.link != c.sentLink || p.view.Warning != nil && p.view.Warning != c.sentWarning) {
		c.sent = false
	}
	if c.details.kind == kindFile {
		c.loadChanges(p.owner)
	}
}

func detailsOf(method string, params jsontext.Value) details {
	if params == nil {
		return details{kind: methodKind(method)}
	}
	return parse(method, params)
}

func methodKind(method string) kind {
	switch method {
	case methodFile:
		return kindFile
	case methodPermission:
		return kindPermission
	case methodQuestion:
		return kindQuestion
	}
	return kindCommand
}

func sameBytes(a, b jsontext.Value) bool {
	if len(a) != len(b) {
		return false
	}
	return len(a) == 0 || &a[0] == &b[0]
}

func (c *card) open(p pending) bool {
	return p.view.Params != nil && p.view.Actionable && !p.view.Answered && !c.sent
}

func (c *card) editable(p pending) bool {
	return c.open(p) && c.locked == ""
}

func (c *card) dirty() bool {
	if c.details.kind != kindQuestion || c.sent {
		return false
	}
	for i := range c.answers {
		if c.answers[i].drafted() {
			return true
		}
	}
	return false
}

func (c *card) status(p pending) string {
	switch {
	case p.view.Err != nil:
		return fmt.Sprintf("Request details failed to load: %v. Nothing was decided; retry to load them.", p.view.Err)
	case p.view.Params == nil:
		return "Decisions stay disabled until the request details are loaded and verified."
	case c.sent || p.view.Answered:
		return c.sentStatus()
	case !p.view.Actionable:
		return "This request is not waiting for an answer from this window."
	case c.details.malformed != nil:
		return fmt.Sprintf("The request details could not be read: %v. Only choices that approve nothing remain available.", c.details.malformed)
	}
	return ""
}

func (c *card) sentStatus() string {
	if c.details.kind == kindQuestion {
		return "Answer sent. dalgona has not confirmed it yet; your answers stay here until it does."
	}
	return "Decision sent. Waiting for dalgona to resolve the request."
}

func (c *card) warningNote(p pending) *warningNote {
	w := p.view.Warning
	if w == nil || c.details.kind != kindQuestion {
		return nil
	}
	return &warningNote{question: correctionTarget(w.Field), reason: w.Reason, message: w.Message}
}

func (c *card) actions(p pending) []action {
	c.acts = append(c.acts[:0], c.base...)
	blocked := ""
	if !c.open(p) {
		blocked = c.status(p)
	}
	for i := range c.acts {
		if blocked != "" {
			c.acts[i].reason = blocked
			continue
		}
		c.acts[i].reason = c.withheld(c.acts[i].gate)
	}
	return c.acts
}

func (c *card) withheld(g gate) string {
	switch {
	case g == gateNone:
		return ""
	case c.details.malformed != nil:
		return "The request details could not be read."
	case g == gateChanges && !c.loaded:
		return "The proposed changes have not arrived in this window yet."
	}
	return ""
}

func decide(decision string) func(*card) jsontext.Value {
	result := decisionResult(decision)
	return func(*card) jsontext.Value { return result }
}

func template(d details) []action {
	escape := action{key: shortcutEscape, variant: variantQuiet}
	switch d.kind {
	case kindCommand, kindFile:
		return []action{
			{label: "Accept", key: shortcutAccept, variant: variantPoint, reply: decide("accept"), gate: approvalGate(d.kind)},
			{label: "Accept for thread", key: shortcutSession, reply: decide("acceptForSession"), gate: approvalGate(d.kind)},
			{label: "Decline", key: shortcutDecline, variant: variantQuiet, reply: decide("decline")},
			with(escape, "Cancel and interrupt", decide("cancel")),
		}
	case kindTrust:
		return []action{
			{label: "Trust workspace", key: shortcutAccept, variant: variantPoint, reply: decide("accept"), gate: gateDetails},
			{label: "Decline", key: shortcutDecline, variant: variantQuiet, reply: decide("decline")},
			with(escape, "Not now", decide("cancel")),
		}
	case kindPermission:
		return []action{
			{label: "Allow once", variant: variantPoint, reply: decide("allowOnce"), gate: gateDetails},
			with(escape, "Decline", decide("decline")),
		}
	case kindSandbox:
		options := d.questions.Questions[0].Options
		return []action{
			{label: options[0].Label, variant: variantPoint, reply: choose(0), gate: gateDetails},
			{label: options[1].Label, variant: variantDestructive, reply: choose(1), gate: gateDetails},
			with(escape, "Cancel", (*card).dismissal),
		}
	case kindQuestion:
	}
	dismiss := with(escape, "Dismiss", (*card).dismissal)
	dismiss.trigger = platform.EditorCancel
	return []action{
		{label: "Submit", variant: variantPoint, reply: (*card).submission, gate: gateDetails, trigger: platform.EditorSubmit},
		{label: "Skip", variant: variantQuiet, reply: (*card).skipping, gate: gateDetails},
		dismiss,
	}
}

func approvalGate(k kind) gate {
	if k == kindFile {
		return gateChanges
	}
	return gateDetails
}

func with(a action, label string, reply func(*card) jsontext.Value) action {
	a.label, a.reply = label, reply
	return a
}

func choose(option int) func(*card) jsontext.Value {
	return func(c *card) jsontext.Value {
		q := c.details.questions.Questions[0]
		return answersResult(map[string]rpc.ToolRequestUserInputAnswer{q.ID: {Answers: []string{q.Options[option].Label}}})
	}
}

func (c *card) dismissal() jsontext.Value { return dismissResult() }
