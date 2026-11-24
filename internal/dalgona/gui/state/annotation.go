package state

import (
	"encoding/json/jsontext"
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	methodRetryWait = "dalgona/retry/wait"
	methodQuestion  = "item/tool/requestUserInput"
	resolvedLimit   = 32
)

type (
	AdvisorNote struct {
		Advisor  string
		Severity string
		Note     string
		Delivery string
	}
	Child struct {
		Thread ThreadKey
		Cwd    string
	}
	Review struct {
		Verdict string
		Summary string
	}
	AnswerWarning struct {
		Message string
		Reason  string
		Field   string
	}
	warningNote struct {
		Message   string `json:"message"`
		RequestID string `json:"requestId,omitzero"`
		Reason    string `json:"reason,omitzero"`
		Field     string `json:"field,omitzero"`
	}
	patchNote struct {
		TurnID  string           `json:"turnId"`
		ItemID  string           `json:"itemId"`
		Changes []rpc.FileChange `json:"changes"`
	}
	childNote struct {
		ID  string
		Cwd string
	}
	ResolvedRequest struct {
		Key          RequestKey
		Method       string
		Item         string
		Resolution   string
		AnsweredHere bool
	}
	resolvedNote struct {
		id           string
		thread       string
		method       string
		item         string
		resolution   string
		answeredHere bool
	}
	itemParams struct {
		ItemID string `json:"itemId"`
	}
)

func (p *projection) applyPlan(raw jsontext.Value) error {
	note, err := decode[rpc.TurnPlanUpdatedNotification](raw)
	if err != nil {
		return err
	}
	p.turn(note.TurnID).plan = note.Plan
	return nil
}

func (p *projection) applyEffort(raw jsontext.Value) error {
	note, err := decode[rpc.TurnEffortNotification](raw)
	if err != nil {
		return err
	}
	p.turn(note.TurnID).effort = note.Effort
	return nil
}

func (p *projection) applyNote(raw jsontext.Value) error {
	note, err := decode[rpc.AdvisorNoteNotification](raw)
	p.notes = append(p.notes, AdvisorNote{Advisor: note.Advisor, Severity: note.Severity, Note: note.Note, Delivery: note.Delivery})
	return err
}

func (p *projection) applyChild(raw jsontext.Value) error {
	note, err := decode[rpc.ChildSpawnedNotification](raw)
	p.children = append(p.children, childNote{ID: note.ChildID, Cwd: note.Cwd})
	return err
}

func (p *projection) applyPatch(raw jsontext.Value) error {
	note, err := decode[patchNote](raw)
	if err != nil {
		return err
	}
	p.turn(note.TurnID).item(note.ItemID).changes = note.Changes
	return nil
}

func (p *projection) applySandbox(raw jsontext.Value) error {
	note, err := decode[rpc.SandboxChosenNotification](raw)
	p.sandbox = note.Choice
	return err
}

func (p *projection) applyWarning(raw jsontext.Value) error {
	note, err := decode[warningNote](raw)
	if err != nil {
		return err
	}
	if note.RequestID == "" {
		p.warning = note.Message
		return nil
	}
	if rq := p.request(note.RequestID); rq != nil {
		rq.warning = &AnswerWarning{Message: note.Message, Reason: note.Reason, Field: note.Field}
	}
	return nil
}

func (p *projection) applyRetryWait(raw jsontext.Value, at time.Time) error {
	note, err := decode[rpc.RetryWaitNotification](raw)
	if err != nil {
		return err
	}
	t := p.turn(note.TurnID)
	t.waitFor, t.waitAt = time.Duration(note.RemainingMS)*time.Millisecond, at
	if note.RemainingMS <= 0 {
		t.waitFor, t.waitAt = 0, time.Time{}
	}
	return nil
}

func (p *projection) resolve(r *request, resolution string) {
	note := resolvedNote{id: r.id, thread: r.owner(p.thread.ID), method: r.method, item: requestItem(r.params), resolution: resolution, answeredHere: r.answered}
	p.resolved = append(p.resolved, note)
	if over := len(p.resolved) - resolvedLimit; over > 0 {
		p.resolved = slices.Delete(p.resolved, 0, over)
	}
	p.lastResolved = &note
}

func requestItem(params jsontext.Value) string {
	if params == nil {
		return ""
	}
	item, err := decode[itemParams](params)
	if err != nil {
		return ""
	}
	return item.ItemID
}

func (p *projection) applyToolReview(raw jsontext.Value) error {
	note, err := decode[rpc.ToolJudgedNotification](raw)
	if err != nil {
		return err
	}
	p.turn(note.TurnID).item(note.ItemID).review = &Review{Verdict: note.Verdict, Summary: note.Summary}
	return nil
}

func (p *projection) applyTurnReview(raw jsontext.Value) error {
	note, err := decode[rpc.TurnJudgedNotification](raw)
	if err != nil {
		return err
	}
	p.turn(note.TurnID).review = &Review{Verdict: note.Verdict, Summary: note.Summary}
	return nil
}
