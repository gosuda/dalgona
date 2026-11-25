//go:build gui

package manage

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"slices"
	"strings"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const boxWorkspace = "/workspace"

type Runner struct {
	base
	target   state.ThreadKey
	status   remoteStatus
	loading  bool
	loaded   bool
	err      error
	epoch    uint64
	more     widget.Clickable
	reload   widget.Clickable
	files    widget.Clickable
	terminal widget.Clickable
	pull     pullRun
	gc       gcRun
}

type gcRun struct {
	preview     mutation
	destroy     mutation
	candidates  *gcResult
	result      *gcResult
	previewBtn  widget.Clickable
	confirmBtn  widget.Clickable
	staleLedger bool
}

func newRunner(h *Hub) *Runner {
	r := &Runner{}
	r.init(h)
	return r
}

func (r *Runner) Opened(route shell.Route) {
	r.reopen()
	r.target = route.Thread
	r.epoch++
	r.status, r.loading, r.loaded, r.err = remoteStatus{}, false, false, nil
	r.pull.reset()
	r.gc = gcRun{}
}

func (r *Runner) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := r.scene(host)
	r.work.settle()
	b := bind(env.View, r.target.Endpoint)
	gate := b.gate(featureRunnerManagement, "runner management")
	r.updateLoad(gtx, b, gate)
	r.updateNav(gtx, env)
	r.pull.update(gtx, pullWork{hub: r.hub, work: &r.work, view: env.View}, b, r.target.Thread)
	r.updateGC(gtx, env, b)
	r.flush(env)
	return r.show(gtx, env, page{title: "Runner", body: r.body(env, b, gate), footer: r.footer(gate)})
}

func (r *Runner) updateLoad(gtx layout.Context, b binding, gate string) {
	if gate == "" && !r.loaded && !r.loading && r.err == nil {
		r.load(b, "")
	}
	if clicked(gtx, &r.more, r.loading || r.status.NextCursor == "") {
		r.load(b, r.status.NextCursor)
	}
	if clicked(gtx, &r.reload, r.loading || gate != "") {
		r.epoch++
		r.status, r.loaded, r.err = remoteStatus{}, false, nil
		r.load(b, "")
	}
}

func (r *Runner) updateNav(gtx layout.Context, env scene) {
	if clicked(gtx, &r.files, false) {
		env.Open(shell.Route{Panel: "files", Thread: r.target})
	}
	if clicked(gtx, &r.terminal, false) {
		env.Open(shell.Route{Panel: "terminal", Thread: r.target})
	}
}

func (r *Runner) footer(gate string) []action {
	return []action{
		{click: &r.files, label: "Browse files", variant: secondary},
		{click: &r.terminal, label: "Open terminal", variant: secondary},
		{click: &r.reload, label: "Reload", variant: quiet, off: r.loading || gate != ""},
	}
}

func (r *Runner) load(b binding, cursor string) {
	r.loading, r.err = true, nil
	epoch, thread := r.epoch, r.target.Thread
	r.work.spawn(func() func() {
		var page remoteStatus
		err := r.hub.call(b, metadataDeadline, methodRemoteStatus, remoteStatusParams{ThreadID: thread, Cursor: cursor}, &page)
		return func() {
			if r.epoch != epoch {
				return
			}
			r.loading, r.err = false, err
			if err != nil {
				return
			}
			r.loaded = true
			r.status.Runners = append(r.status.Runners, page.Runners...)
			r.status.Bindings = append(r.status.Bindings, page.Bindings...)
			r.status.LedgerRevision, r.status.NextCursor = page.LedgerRevision, page.NextCursor
		}
	})
}

func (r *Runner) binding() *remoteBinding {
	at := slices.IndexFunc(r.status.Bindings, func(b remoteBinding) bool { return b.ThreadID == r.target.Thread })
	if at < 0 {
		return nil
	}
	return &r.status.Bindings[at]
}

func (r *Runner) body(env scene, b binding, gate string) []layout.Widget {
	body := []layout.Widget{
		sayer(env, toneHeading, threadTitle(env.View, r.target)),
		targetLine(env, r.target),
	}
	if gate != "" {
		return append(body, sayer(env, toneWarning, gate))
	}
	switch {
	case r.err != nil:
		return append(body, sayer(env, toneFailed, "Runner status did not load: "+explain(r.err)))
	case !r.loaded:
		return append(body, sayer(env, toneDim, "Loading runner status from "+string(r.target.Endpoint)+"…"))
	}
	body = append(body, r.modeWidgets(env)...)
	body = append(body, r.inventory(env)...)
	if bound := r.binding(); bound != nil && bound.State == "bound" && bound.BoxID != "" {
		body = append(body, sayer(env, toneHeading, "Pull"))
		body = append(body, r.pull.widgets(env, b, threadCwd(env.View, r.target))...)
	}
	return append(body, r.gcWidgets(env)...)
}

func (r *Runner) modeWidgets(env scene) []layout.Widget {
	bound := r.binding()
	cwd := cmpOr(threadCwd(env.View, r.target), "unknown")
	ep, _ := endpointView(env.View, r.target.Endpoint)
	out := []layout.Widget{sayer(env, toneBody, modeText(ep.Kind, bound, r.target.Endpoint))}
	root := "Workspace root: " + cwd
	if bound != nil && bound.BoxID != "" {
		root += " on this endpoint, mapped to " + boxWorkspace + " in box " + bound.BoxID
	}
	out = append(out, sayer(env, toneCode, root))
	if bound != nil {
		out = append(out, sayer(env, toneDim, archiveText(*bound)))
	}
	return append(out, sayer(env, toneDim, fmt.Sprintf("Runner configuration changes on %s apply to threads created afterwards; this open thread keeps its current binding.", r.target.Endpoint)))
}

func modeText(kind client.EndpointKind, bound *remoteBinding, endpoint client.EndpointID) string {
	prefix := ""
	if kind != client.EndpointLocal {
		prefix = fmt.Sprintf("Remote daemon: the daemon on %s owns this thread. ", endpoint)
	}
	switch {
	case bound != nil && bound.BoxID != "" && bound.State == "bound":
		return prefix + fmt.Sprintf("Remote archive execution: tools run in box %s on runner %s.", bound.BoxID, bound.RunnerID)
	case bound != nil && bound.State == "destroyed":
		return prefix + fmt.Sprintf("The box of runner %s was destroyed; this thread cannot run its tools remotely.", bound.RunnerID)
	}
	return prefix + fmt.Sprintf("Local execution: tools run on the host of %s.", endpoint)
}

func archiveText(b remoteBinding) string {
	shared := "no other thread shares it"
	if len(b.SharedThreadIDs) > 0 {
		shared = "shared with " + strings.Join(b.SharedThreadIDs, ", ")
	}
	return fmt.Sprintf("Binding %s on runner %s, %s; binding revision %s.", b.State, cmpOr(b.RunnerID, "local"), shared, cmpOr(b.BindingRevision, "none"))
}

func (r *Runner) inventory(env scene) []layout.Widget {
	out := []layout.Widget{sayer(env, toneHeading, "Runners on "+string(r.target.Endpoint))}
	for _, rn := range r.status.Runners {
		availability := "available"
		if !rn.Available {
			availability = "unavailable: its service cannot be built"
		}
		out = append(out, sayer(env, toneBody, fmt.Sprintf("%s · %s · %s · %s", rn.Name, cmpOr(rn.Service, "local"), availability, cmpOr(strings.Join(rn.Capabilities, ", "), "no extra capabilities"))))
	}
	if r.status.NextCursor != "" {
		out = append(out, buttons(env, action{click: &r.more, label: "Load more runners", variant: quiet, off: r.loading}))
	}
	return out
}

func (r *Runner) updateGC(gtx layout.Context, env scene, b binding) {
	g := &r.gc
	busy := g.preview.running() || g.destroy.running()
	if clicked(gtx, &g.previewBtn, busy || !b.live) {
		r.runGC(b, &g.preview, gcParams{Yes: false})
	}
	if clicked(gtx, &g.confirmBtn, busy || g.candidates == nil || len(g.candidates.Candidates) == 0 || g.candidates.Truncated) {
		r.runGC(b, &g.destroy, gcParams{Yes: true, PreviewRevision: g.candidates.PreviewRevision})
	}
	r.hub.checkOutcome(gtx, &r.work, env.View, &g.destroy, func(raw jsontext.Value) { r.gcDone(&g.destroy, raw, nil) })
}

func (r *Runner) runGC(b binding, m *mutation, p gcParams) {
	id := m.begin(methodRemoteGC, b, false)
	r.gc.staleLedger = false
	r.work.spawn(func() func() {
		var raw jsontext.Value
		err := r.hub.mutate(operationRequest{binding: b, deadline: longDeadline, method: methodRemoteGC, operationID: id, params: p, result: &raw})
		return func() {
			if m.id != id {
				return
			}
			m.settle(err)
			r.gcDone(m, raw, err)
		}
	})
}

func (r *Runner) gcDone(m *mutation, raw jsontext.Value, err error) {
	g := &r.gc
	if kindOf(err) == rpc.ErrorStaleRevision {
		g.staleLedger, g.candidates = true, nil
		return
	}
	var res gcResult
	if err != nil || json.Unmarshal(raw, &res) != nil {
		return
	}
	if m == &g.preview {
		g.candidates, g.result = &res, nil
		return
	}
	g.result, g.candidates = &res, nil
	r.announce(platform.AnnounceCompletion, fmt.Sprintf("Garbage collection on %s removed %d boxes", r.target.Endpoint, len(res.Destroyed)))
}

func (r *Runner) gcWidgets(env scene) []layout.Widget {
	g := &r.gc
	out := []layout.Widget{
		sayer(env, toneHeading, "Garbage collection"),
		sayer(env, toneDim, fmt.Sprintf("Preview lists boxes on %s that no live thread uses. Nothing is removed until you confirm that exact list.", r.target.Endpoint)),
		buttons(env, action{click: &g.previewBtn, label: "Preview removable boxes", variant: secondary, off: g.preview.running() || g.destroy.running()}),
		g.preview.line(env),
	}
	if g.staleLedger {
		out = append(out, sayer(env, toneWarning, "The box ledger changed since the preview; nothing was removed. Preview again."))
	}
	if g.candidates != nil {
		out = append(out, candidateWidgets(env, r.target.Endpoint, g)...)
	}
	out = append(out, g.destroy.line(env))
	if g.result != nil {
		out = append(out, gcResultWidgets(env, *g.result)...)
	}
	return out
}

func candidateWidgets(env scene, endpoint client.EndpointID, g *gcRun) []layout.Widget {
	c := g.candidates
	if len(c.Candidates) == 0 {
		return []layout.Widget{sayer(env, toneBody, "No box is eligible for removal on "+string(endpoint)+".")}
	}
	out := []layout.Widget{sayer(env, toneWarning, fmt.Sprintf("Confirming removes these %d boxes on %s and every file inside them:", len(c.Candidates), endpoint))}
	for _, cand := range c.Candidates {
		out = append(out, sayer(env, toneCode, fmt.Sprintf("%s on runner service %s · threads %s · %s", cand.BoxID, cand.Service, cmpOr(strings.Join(cand.ThreadIDs, ", "), "none"), cand.Reason)))
	}
	if c.Truncated {
		out = append(out, sayer(env, toneFailed, "This preview is truncated: the daemon did not disclose every removable box, so confirmation stays disabled rather than removing an unknown scope."))
		return out
	}
	label := fmt.Sprintf("Remove %d boxes on %s", len(c.Candidates), endpoint)
	return append(out, buttons(env, action{click: &g.confirmBtn, label: label, variant: destructive, off: g.destroy.running()}))
}

func gcResultWidgets(env scene, res gcResult) []layout.Widget {
	out := []layout.Widget{sayer(env, toneBody, fmt.Sprintf("The daemon removed %d boxes: %s.", len(res.Destroyed), cmpOr(strings.Join(res.Destroyed, ", "), "none")))}
	for _, f := range res.Failures {
		out = append(out, sayer(env, toneFailed, fmt.Sprintf("Box %s was not removed (%s).", f.BoxID, f.Kind)))
	}
	if res.Truncated {
		out = append(out, sayer(env, toneWarning, "The lists above are truncated at 1000 entries."))
	}
	return out
}
