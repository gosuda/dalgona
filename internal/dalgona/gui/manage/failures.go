//go:build gui

package manage

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"strings"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	failuresPageLimit = 50
	consentWord       = "yes"
	reporterTarget    = "failure-reporting"
	ghNotice          = "Gh mode reads a token once through `gh auth token --hostname github.com --no-refresh` each time it reports; dalgona never logs in, refreshes, or borrows another credential."
)

var confirmPolicy = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: 16}}

type Failures struct {
	base
	endpoint client.EndpointID
	pick     choice
	epoch    uint64
	records  []failureSummary
	cursor   string
	loading  bool
	listErr  error
	loaded   bool
	more     widget.Clickable
	reload   widget.Clickable
	open     choice
	record   *failureRecord
	raw      jsontext.Value
	readErr  error
	reading  bool
	export   widget.Clickable
	exported string
	expErr   error
	report   reportingPanel
}

type reportingPanel struct {
	state    *reportingState
	err      error
	loading  bool
	method   string
	methods  choice
	confirm  *field
	enable   mutation
	disable  mutation
	enableB  widget.Clickable
	disableB widget.Clickable
	result   string
	auth     *authFlow
}

func newFailures(h *Hub) *Failures {
	f := &Failures{report: reportingPanel{confirm: newField(confirmPolicy, false), auth: newAuthFlow()}}
	f.init(h)
	return f
}

func (f *Failures) Opened(route shell.Route) {
	f.reopen()
	if route.Thread.Endpoint != "" && route.Thread.Endpoint != f.endpoint {
		f.endpoint = route.Thread.Endpoint
		f.restart()
	}
}

func (f *Failures) restart() {
	f.epoch++
	f.records, f.cursor, f.loading, f.listErr, f.loaded = nil, "", false, nil, false
	f.record, f.raw, f.readErr, f.reading, f.exported, f.expErr = nil, nil, nil, false, "", nil
	f.report = reportingPanel{confirm: newField(confirmPolicy, false), auth: newAuthFlow()}
}

func (f *Failures) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := f.scene(host)
	f.work.settle()
	f.chooseEndpoint(gtx, env)
	b := bind(env.View, f.endpoint)
	if b.gate(featureFailures, "failure records") == "" && !f.loaded && !f.loading && f.listErr == nil {
		f.list(b, "")
	}
	if b.gate(featureReporting, "reporting") == "" && f.report.state == nil && !f.report.loading && f.report.err == nil {
		f.readReporting(b, "")
	}
	f.updateList(gtx, b)
	f.updateReporting(gtx, env, b)
	f.flush(env)
	return f.show(gtx, env, page{title: "Failures and reporting", body: f.body(env, b), footer: []action{{click: &f.reload, label: "Reload", variant: quiet, off: f.loading}}})
}

func (f *Failures) chooseEndpoint(gtx layout.Context, env scene) {
	eps := env.View.Endpoints()
	if f.endpoint == "" && len(eps) > 0 {
		f.endpoint = cmpEndpoint(env.Thread.Endpoint, eps[0].ID)
	}
	if at := f.pick.picked(gtx, len(eps)); at >= 0 && eps[at].ID != f.endpoint {
		f.endpoint = eps[at].ID
		f.restart()
	}
}

func (f *Failures) list(b binding, cursor string) {
	f.loading, f.listErr = true, nil
	epoch := f.epoch
	f.work.spawn(func() func() {
		var page failuresPage
		err := f.hub.call(b, metadataDeadline, methodFailuresList, failuresParams{Cursor: cursor, Limit: failuresPageLimit}, &page)
		return func() {
			if f.epoch != epoch {
				return
			}
			f.loading, f.listErr = false, err
			if err == nil {
				f.loaded, f.cursor = true, page.NextCursor
				f.records = append(f.records, page.Records...)
			}
		}
	})
}

func (f *Failures) updateList(gtx layout.Context, b binding) {
	if clicked(gtx, &f.reload, f.loading) {
		f.restart()
	}
	if clicked(gtx, &f.more, f.loading || f.cursor == "") {
		f.list(b, f.cursor)
	}
	if at := f.open.picked(gtx, len(f.records)); at >= 0 && !f.reading {
		f.read(b, f.records[at].ID)
	}
	if clicked(gtx, &f.export, f.raw == nil) {
		f.exportRecord()
	}
}

func (f *Failures) read(b binding, id string) {
	f.reading, f.readErr, f.record, f.raw, f.exported, f.expErr = true, nil, nil, nil, "", nil
	epoch := f.epoch
	f.work.spawn(func() func() {
		var raw jsontext.Value
		err := f.hub.call(b, metadataDeadline, methodFailuresRead, failureID{ID: id}, &raw)
		var rec failureRecord
		if err == nil {
			err = json.Unmarshal(raw, &rec)
		}
		return func() {
			if f.epoch != epoch {
				return
			}
			f.reading, f.readErr = false, err
			if err == nil {
				f.record, f.raw = &rec, raw
			}
		}
	})
}

func (f *Failures) exportRecord() {
	raw, id := f.raw, f.record.ID
	f.work.spawn(func() func() {
		err := f.hub.export("dalgona-failure-"+id+".json", raw)
		return func() {
			f.expErr = err
			if err == nil {
				f.exported = "Exported the masked record. Exporting never enables reporting."
			}
		}
	})
}

func (f *Failures) body(env scene, b binding) []layout.Widget {
	out := []layout.Widget{f.endpointChoice(env)}
	if gate := b.gate(featureFailures, "failure records"); gate != "" {
		out = append(out, sayer(env, toneWarning, gate))
	} else {
		out = append(out, f.recordWidgets(env)...)
	}
	out = append(out, sayer(env, toneHeading, "Reporting on "+string(f.endpoint)))
	if gate := b.gate(featureReporting, "failure reporting"); gate != "" {
		return append(out, sayer(env, toneWarning, gate))
	}
	return append(out, f.reportingWidgets(env, b)...)
}

func (f *Failures) endpointChoice(env scene) layout.Widget {
	eps := env.View.Endpoints()
	f.pick.grow(len(eps))
	return func(gtx layout.Context) layout.Dimensions {
		items := make([]layout.Widget, len(eps))
		for i, e := range eps {
			label := fmt.Sprintf("Failures on %s · %s", e.ID, phaseWord(e.Phase))
			items[i] = func(gtx layout.Context) layout.Dimensions {
				return option(gtx, env, optionSpec{click: &f.pick.clicks[i], label: label, selected: e.ID == f.endpoint, content: sayer(env, toneBody, label)})
			}
		}
		return flow(gtx, theme.Space2, items...)
	}
}

func (f *Failures) recordWidgets(env scene) []layout.Widget {
	var out []layout.Widget
	switch {
	case f.listErr != nil:
		return []layout.Widget{sayer(env, toneFailed, "Failure records did not load: "+explain(f.listErr))}
	case f.loading && len(f.records) == 0:
		return []layout.Widget{sayer(env, toneDim, "Loading failure records…")}
	case f.loaded && len(f.records) == 0:
		out = append(out, sayer(env, toneBody, "No failure records on "+string(f.endpoint)+"."))
	}
	f.open.grow(len(f.records))
	for i, r := range f.records {
		label := fmt.Sprintf("%s · %s · %s · %s", r.Time, r.Class, cmpOr(r.Tool, "no tool"), recordStatus(r))
		out = append(out, buttons(env, action{click: &f.open.clicks[i], label: label, variant: quiet}))
	}
	if f.cursor != "" {
		out = append(out, buttons(env, action{click: &f.more, label: "Load more records", variant: quiet, off: f.loading}))
	}
	return append(out, f.viewer(env)...)
}

func recordStatus(r failureSummary) string {
	switch {
	case r.ContentFree:
		return "content removed"
	case r.Reportable:
		return "reportable"
	}
	return "local only"
}

func (f *Failures) viewer(env scene) []layout.Widget {
	switch {
	case f.reading:
		return []layout.Widget{sayer(env, toneDim, "Reading the record…")}
	case f.readErr != nil:
		return []layout.Widget{sayer(env, toneFailed, readFailure(f.readErr))}
	case f.record == nil:
		return nil
	}
	r := f.record
	out := []layout.Widget{
		sayer(env, toneHeading, fmt.Sprintf("Record %s on %s", r.ID, f.endpoint)),
		sayer(env, toneDim, fmt.Sprintf("%s · %s · %s", r.Time, r.Class, cmpOr(r.Tool, "no tool"))),
	}
	if r.ContentFree {
		out = append(out, sayer(env, toneWarning, "This record's content was removed by masking; only its metadata remains."))
	}
	out = append(out, sayer(env, toneCode, excerpt("Error", r.Error)), sayer(env, toneCode, excerpt("Output", r.Output)), sayer(env, toneCode, excerpt("Arguments", r.Args)))
	if len(r.Stack) > 0 {
		lines := make([]string, len(r.Stack))
		for i, fr := range r.Stack {
			lines[i] = fmt.Sprintf("%s %s:%d", fr.Function, fr.File, fr.Line)
		}
		out = append(out, sayer(env, toneCode, strings.Join(lines, "\n")))
	}
	out = append(out, buttons(env, action{click: &f.export, label: "Export record", variant: secondary}))
	if f.exported != "" {
		out = append(out, sayer(env, toneAdded, f.exported))
	}
	if f.expErr != nil {
		out = append(out, sayer(env, toneFailed, "Export failed: "+f.expErr.Error()))
	}
	return out
}

func readFailure(err error) string {
	if kindOf(err) == rpc.ErrorNotFound {
		return "The record is gone; it was removed after the list loaded."
	}
	return "The record did not load: " + explain(err)
}

func excerpt(name, text string) string {
	if text == "" {
		return name + ": unavailable"
	}
	return name + ": " + text
}

func (f *Failures) readReporting(b binding, method string) {
	r := &f.report
	r.loading, r.err = true, nil
	epoch := f.epoch
	f.work.spawn(func() func() {
		var st reportingState
		err := f.hub.call(b, metadataDeadline, methodReportingRead, reportingReadParams{Method: method}, &st)
		return func() {
			if f.epoch != epoch {
				return
			}
			r.loading, r.err = false, err
			if err == nil {
				r.state, r.method = &st, st.Method
				r.confirm.set("")
			}
		}
	})
}

func (f *Failures) updateReporting(gtx layout.Context, env scene, b binding) {
	r := &f.report
	r.confirm.update(gtx)
	methods := []string{"app", "gh"}
	if at := r.methods.picked(gtx, len(methods)); at >= 0 && r.state != nil && methods[at] != r.method && !r.enable.running() {
		f.readReporting(b, methods[at])
	}
	f.updateConsent(gtx, b, r)
	f.hub.checkOutcome(gtx, &f.work, env.View, &r.enable, func(jsontext.Value) { f.readReporting(b, r.method) })
	f.hub.checkOutcome(gtx, &f.work, env.View, &r.disable, func(jsontext.Value) { f.readReporting(b, r.method) })
	f.updateReportAuth(gtx, env, b, r)
}

func (f *Failures) updateConsent(gtx layout.Context, b binding, r *reportingPanel) {
	ready := r.state != nil && r.state.Available && r.confirm.text() == consentWord
	if clicked(gtx, &r.enableB, !ready || r.enable.running()) {
		f.enable(b)
	}
	if clicked(gtx, &r.disableB, r.state == nil || r.disable.running()) {
		f.disable(b)
	}
}

func (f *Failures) updateReportAuth(gtx layout.Context, env scene, b binding, r *reportingPanel) {
	if r.state == nil || r.method != "app" {
		return
	}
	if r.auth.target.ID != reporterTarget {
		r.auth.bind(authTarget{Kind: "reporter", ID: reporterTarget}, "Reporter App")
	}
	ep, _ := endpointView(env.View, f.endpoint)
	r.auth.update(gtx, authWork{hub: f.hub, work: &f.work, view: env.View, bind: b}, ep.Kind)
	if r.auth.reloadClicked(gtx) || r.auth.desc == nil && !r.auth.loading && r.auth.loadErr == nil {
		r.auth.load(f.hub, &f.work, b, "")
	}
}

func (f *Failures) enable(b binding) {
	r := &f.report
	id := r.enable.begin(methodReportingEnable, b, true)
	params := enableParams{Method: r.method, TextHash: r.state.TextHash, Confirmation: r.confirm.text(), OperationID: id}
	f.work.spawn(func() func() {
		var res enabled
		err := f.hub.call(b, pageDeadline, methodReportingEnable, params, &res)
		return func() {
			if r.enable.id != id {
				return
			}
			r.enable.settle(err)
			if kindOf(err) == rpc.ErrorStaleRevision {
				r.result = "The consent text changed; read the new text and type yes again."
			}
			if err == nil {
				r.result = fmt.Sprintf("Reporting is on with method %s. Consent receipt stored %s.", res.ConsentReceipt.Method, res.ConsentReceipt.Time)
				f.announce(platform.AnnounceCompletion, "Failure reporting enabled")
			}
			f.readReporting(b, r.method)
		}
	})
}

func (f *Failures) disable(b binding) {
	r := &f.report
	id := r.disable.begin(methodReportingDisable, b, true)
	f.work.spawn(func() func() {
		var res disabled
		err := f.hub.call(b, pageDeadline, methodReportingDisable, operationOnly{OperationID: id}, &res)
		return func() {
			if r.disable.id != id {
				return
			}
			r.disable.settle(err)
			if err == nil {
				r.result = disabledText(res)
				f.announce(platform.AnnounceCompletion, "Failure reporting disabled")
			}
			f.readReporting(b, r.method)
		}
	})
}

func disabledText(res disabled) string {
	text := "Reporting is off. Consent, App credentials, and queued excerpts were removed."
	if !res.QueuedReportsDeleted {
		text = "Reporting is off, but the daemon did not confirm that queued excerpts were removed."
	}
	return text + " Issues already published on GitHub stay published."
}

func (f *Failures) reportingWidgets(env scene, b binding) []layout.Widget {
	r := &f.report
	switch {
	case r.err != nil:
		return []layout.Widget{sayer(env, toneFailed, "Reporting state did not load: "+explain(r.err))}
	case r.state == nil:
		return []layout.Widget{sayer(env, toneDim, "Loading reporting state…")}
	}
	st := r.state
	out := []layout.Widget{f.methodChoice(env), sayer(env, toneBody, reportingSummary(*st))}
	if st.Restriction != nil {
		out = append(out, sayer(env, toneWarning, restrictionText(*st.Restriction)))
	}
	if st.UncertainCount > 0 {
		out = append(out, sayer(env, toneWarning, fmt.Sprintf("%d reports have an uncertain delivery; they are never sent again automatically.", st.UncertainCount)))
	}
	if r.method == "gh" {
		out = append(out, sayer(env, toneDim, ghNotice))
	}
	out = append(out, f.consentWidgets(env, *st)...)
	if r.method == "app" {
		out = append(out, sayer(env, toneHeading, "Reporter App sign-in"))
		ep, _ := endpointView(env.View, f.endpoint)
		out = append(out, r.auth.widgets(env, ep.Kind)...)
	}
	out = append(out, r.enable.line(env), r.disable.line(env))
	if r.result != "" {
		out = append(out, sayer(env, toneBody, r.result))
	}
	return out
}

func reportingSummary(st reportingState) string {
	state := "off"
	if st.Enabled {
		state = "on"
	}
	return fmt.Sprintf("Reporting is %s with method %s. Authorization: %s. Queued reports: %d.", state, st.Method, st.Authorization, st.QueueCount)
}

func restrictionText(r restriction) string {
	text := "Reporting is restricted (" + r.Kind + "): " + r.Message
	if r.Until != "" {
		text += " Until " + r.Until + "."
	}
	return text
}

func (f *Failures) methodChoice(env scene) layout.Widget {
	r := &f.report
	methods := []string{"app", "gh"}
	r.methods.grow(len(methods))
	return func(gtx layout.Context) layout.Dimensions {
		items := make([]layout.Widget, len(methods))
		for i, m := range methods {
			label := "Report with " + m
			items[i] = func(gtx layout.Context) layout.Dimensions {
				return option(gtx, env, optionSpec{click: &r.methods.clicks[i], label: label, selected: m == r.method, content: sayer(env, toneBody, label)})
			}
		}
		return flow(gtx, theme.Space2, items...)
	}
}

func (f *Failures) consentWidgets(env scene, st reportingState) []layout.Widget {
	r := &f.report
	disable := buttons(env, action{click: &r.disableB, label: "Disable reporting", variant: destructive, off: r.disable.running()})
	removal := sayer(env, toneDim, "Disabling removes the consent receipt, Reporter App credentials, and queued excerpts. Issues already published on GitHub stay published.")
	if st.Enabled {
		return []layout.Widget{removal, disable}
	}
	if !st.Available {
		return []layout.Widget{sayer(env, toneWarning, "Reporting is unavailable on this daemon with method "+st.Method+"; nothing can enable it here."), removal, disable}
	}
	return []layout.Widget{
		sayer(env, toneBody, st.ConsentText),
		func(gtx layout.Context) layout.Dimensions {
			return r.confirm.layout(gtx, env, "Type yes to agree to the text above", r.enable.running())
		},
		buttons(env, action{click: &r.enableB, label: "Enable reporting", variant: pointed, off: r.confirm.text() != consentWord || r.enable.running()}),
		removal, disable,
	}
}
