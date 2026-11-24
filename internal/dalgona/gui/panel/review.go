//go:build gui

package panel

import (
	"regexp"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	lineLimit         = 4096
	instructionsLimit = 15 << 20
	reviewUncommitted = "uncommittedChanges"
	reviewBranch      = "baseBranch"
	reviewCommit      = "commit"
	reviewCustom      = "custom"
)

var (
	linePolicy         = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: lineLimit}}
	instructionsPolicy = inputtext.Policy{Lines: inputtext.MultiLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: instructionsLimit}}
	branchGrammar      = regexp.MustCompile(`^[A-Za-z0-9._/][A-Za-z0-9._/-]*$`)
	commitGrammar      = regexp.MustCompile(`^[0-9a-fA-F]{4,64}$`)
	reviewKinds        = [...]struct{ kind, label string }{
		{reviewUncommitted, "Uncommitted changes"}, {reviewBranch, "Base branch"}, {reviewCommit, "Commit"}, {reviewCustom, "Custom"},
	}
)

type reviewPanel struct {
	thread       state.ThreadKey
	kind         string
	choices      [len(reviewKinds)]Choice
	branch       field
	commit       field
	instructions field
	op           operation
}

func (r *reviewPanel) open(route shell.Route) {
	r.thread = route.Thread
	r.kind = reviewUncommitted
	r.branch = newField("Base branch name", linePolicy)
	r.commit = newField("Commit id", linePolicy)
	r.instructions = newField("Review instructions", instructionsPolicy)
	r.op.reset()
}

func (r *reviewPanel) release(out *[]state.Intent) { r.op.call.release(out) }

func (r *reviewPanel) target() (rpc.ReviewTarget, string) {
	switch r.kind {
	case reviewBranch:
		return checked(rpc.ReviewTarget{Type: r.kind, Branch: r.branch.text()}, r.branch.text(), branchGrammar,
			"Enter a base branch.", "A branch name uses letters, digits, dot, underscore, slash and hyphen, and cannot start with a hyphen.")
	case reviewCommit:
		return checked(rpc.ReviewTarget{Type: r.kind, SHA: r.commit.text()}, r.commit.text(), commitGrammar,
			"Enter a commit id.", "A commit id is 4 to 64 hexadecimal digits.")
	case reviewCustom:
		if inputtext.Blank(r.instructions.text()) {
			return rpc.ReviewTarget{}, "Enter review instructions."
		}
		return rpc.ReviewTarget{Type: r.kind, Instructions: r.instructions.text()}, ""
	}
	return rpc.ReviewTarget{Type: reviewUncommitted}, ""
}

func checked(t rpc.ReviewTarget, value string, grammar *regexp.Regexp, blank, malformed string) (rpc.ReviewTarget, string) {
	if inputtext.Blank(value) {
		return rpc.ReviewTarget{}, blank
	}
	if !grammar.MatchString(value) {
		return rpc.ReviewTarget{}, malformed
	}
	return t, ""
}

func (r *reviewPanel) start(s *scene) {
	t, problem := r.target()
	if problem != "" {
		r.op.err = problem
		return
	}
	r.op.start(s, request{endpoint: r.thread.Endpoint, method: "review/start", params: rpc.ReviewStartParams{ThreadID: r.thread.Thread, Target: t}})
}

func (r *reviewPanel) fields(gtx layout.Context, s *scene) bool {
	submit := false
	for _, f := range []*field{&r.branch, &r.commit, &r.instructions} {
		ev := f.update(gtx)
		if ev.cancel {
			s.close()
		}
		if ev.changed {
			r.op.err = ""
		}
		submit = submit || ev.submit && f != &r.instructions
	}
	return submit
}

func (r *reviewPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	if r.thread.Thread == "" {
		return noThread(gtx, s, "review")
	}
	r.pickKind(gtx)
	submit := r.fields(gtx, s)
	if (r.op.run.Clicked(gtx) || submit) && !r.op.call.running() {
		r.start(s)
	}
	if v, ok := r.op.settle(s); ok {
		r.follow(s, v)
	}
	content := []layout.Widget{targetLine(s, r.thread), s.legend("Review target")}
	for i, k := range reviewKinds {
		content = append(content, func(gtx layout.Context) layout.Dimensions {
			return r.choices[i].Layout(gtx, s.frame, s.env, Option{Label: k.label, Selected: r.kind == k.kind})
		})
		if f := r.fieldFor(k.kind); f != nil && r.kind == k.kind {
			content = append(content, func(gtx layout.Context) layout.Dimensions { return f.layout(gtx, s) })
		}
	}
	content = append(content, r.op.status(s, "Starting the review")...)
	n, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["review"], Rows: n, Row: row, Dismiss: "Cancel",
		Actions: []layout.Widget{r.op.button(s, "Start review", "Starting", Point)},
	})
}

func (r *reviewPanel) pickKind(gtx layout.Context) {
	for i := range r.choices {
		if r.choices[i].Clicked(gtx) {
			r.kind, r.op.err = reviewKinds[i].kind, ""
		}
	}
}

func (r *reviewPanel) fieldFor(kind string) *field {
	switch kind {
	case reviewBranch:
		return &r.branch
	case reviewCommit:
		return &r.commit
	case reviewCustom:
		return &r.instructions
	}
	return nil
}

func (r *reviewPanel) follow(s *scene, v state.CallView) {
	started, err := decode[rpc.ReviewStartResponse](v)
	s.announce(platform.AnnounceCompletion, "Review started")
	s.close()
	if err != nil || started.ReviewThreadID == "" || started.ReviewThreadID == r.thread.Thread {
		s.env.Submit(state.Refresh{Thread: r.thread})
		return
	}
	if s.env.Navigate != nil {
		s.env.Navigate(state.ThreadKey{Endpoint: r.thread.Endpoint, Thread: started.ReviewThreadID})
	}
}
