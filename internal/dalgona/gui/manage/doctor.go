//go:build gui

package manage

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strings"

	"gioui.org/io/clipboard"
	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type diagnosticRun struct {
	endpoint client.EndpointID
	online   bool
	local    bool
	rows     []DiagnosticRow
	err      error
	running  bool
	stop     context.CancelFunc
	serial   uint64
}

type Diagnostics struct {
	base
	endpoint client.EndpointID
	pick     choice
	run      diagnosticRun
	serial   uint64
	offline  widget.Clickable
	online   widget.Clickable
	cancel   widget.Clickable
	copyBtn  widget.Clickable
	export   widget.Clickable
	copied   string
	exported string
	expErr   error
}

func newDiagnostics(h *Hub) *Diagnostics {
	d := &Diagnostics{}
	d.init(h)
	return d
}

func (d *Diagnostics) Opened(route shell.Route) {
	d.reopen()
	if route.Thread.Endpoint != "" && route.Thread.Endpoint != d.endpoint {
		d.endpoint, d.run = route.Thread.Endpoint, diagnosticRun{}
	}
}

func (d *Diagnostics) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := d.scene(host)
	d.work.settle()
	d.chooseEndpoint(gtx, env)
	b := bind(env.View, d.endpoint)
	if clicked(gtx, &d.offline, d.run.running || d.blocked(env, b) != "") {
		d.start(env, b, false)
	}
	if clicked(gtx, &d.online, d.run.running || d.blocked(env, b) != "") {
		d.start(env, b, true)
	}
	if clicked(gtx, &d.cancel, !d.run.running) {
		d.run.stop()
	}
	if clicked(gtx, &d.copyBtn, len(d.run.rows) == 0) {
		d.copied = d.report()
	}
	if clicked(gtx, &d.export, len(d.run.rows) == 0) {
		d.exportReport()
	}
	if d.copied != "" {
		gtx.Execute(clipboard.WriteCmd{Type: "application/text", Data: io.NopCloser(strings.NewReader(d.copied))})
		d.copied = ""
		d.exported = "Copied the masked diagnostics."
	}
	d.flush(env)
	return d.show(gtx, env, page{title: "Diagnostics", body: d.body(env, b), footer: d.footer(env, b)})
}

func (d *Diagnostics) chooseEndpoint(gtx layout.Context, env scene) {
	eps := env.View.Endpoints()
	if d.endpoint == "" && len(eps) > 0 {
		d.endpoint = cmpEndpoint(env.Thread.Endpoint, eps[0].ID)
	}
	if at := d.pick.picked(gtx, len(eps)); at >= 0 && eps[at].ID != d.endpoint && !d.run.running {
		d.endpoint, d.run = eps[at].ID, diagnosticRun{}
	}
}

func (d *Diagnostics) local(env scene) bool {
	e, _ := endpointView(env.View, d.endpoint)
	return e.Kind == client.EndpointLocal
}

func (d *Diagnostics) blocked(env scene, b binding) string {
	switch {
	case b.live:
		return b.gate(featureDiagnostics, "diagnostics")
	case d.local(env):
		return ""
	}
	return fmt.Sprintf("%s is %s. Remote diagnostics need that endpoint connected; local results never stand in for it.", d.endpoint, phaseWord(b.phase))
}

func (d *Diagnostics) start(env scene, b binding, online bool) {
	d.serial++
	ctx, stop := context.WithTimeout(d.hub.ctx, pageDeadline)
	d.run = diagnosticRun{endpoint: d.endpoint, online: online, local: !b.live, running: true, stop: stop, serial: d.serial}
	serial, endpoint := d.serial, d.endpoint
	d.work.spawn(func() func() {
		defer stop()
		rows, err := d.diagnose(ctx, b, endpoint, online)
		return func() {
			if d.run.serial != serial {
				return
			}
			d.run.running, d.run.rows, d.run.err = false, rows, err
			d.announce(platform.AnnounceCompletion, "Diagnostics finished for "+string(endpoint))
		}
	})
}

func (d *Diagnostics) diagnose(ctx context.Context, b binding, endpoint client.EndpointID, online bool) ([]DiagnosticRow, error) {
	if !b.live {
		return d.hub.deps.Lifecycle.Diagnose(ctx, endpoint, online)
	}
	var res diagnosticsResult
	if err := d.hub.callContext(ctx, operationRequest{binding: b, deadline: pageDeadline, method: methodDiagnosticsRun, params: diagnosticsParams{Online: online}, result: &res}); err != nil {
		return nil, err
	}
	rows := make([]DiagnosticRow, len(res.Rows))
	for i, r := range res.Rows {
		rows[i] = DiagnosticRow(r)
	}
	return rows, nil
}

func (d *Diagnostics) footer(env scene, b binding) []action {
	off := d.run.running || d.blocked(env, b) != ""
	return []action{
		{click: &d.offline, label: "Run local checks", variant: pointed, off: off},
		{click: &d.online, label: "Run online checks", variant: secondary, off: off},
	}
}

func (d *Diagnostics) body(env scene, b binding) []layout.Widget {
	out := []layout.Widget{
		d.endpointChoice(env),
		sayer(env, toneBody, "Local checks read configuration, credentials, and the daemon's state without contacting anyone. Online checks also send one bounded model-list request to each configured provider."),
		sayer(env, toneDim, "Neither run starts a daemon, repairs configuration, or changes credentials. Checks start only when you choose one."),
	}
	if reason := d.blocked(env, b); reason != "" {
		out = append(out, sayer(env, toneWarning, reason))
	}
	return append(out, d.results(env)...)
}

func (d *Diagnostics) endpointChoice(env scene) layout.Widget {
	eps := env.View.Endpoints()
	d.pick.grow(len(eps))
	return func(gtx layout.Context) layout.Dimensions {
		items := make([]layout.Widget, len(eps))
		for i, e := range eps {
			label := fmt.Sprintf("Diagnose %s · %s", e.ID, phaseWord(e.Phase))
			items[i] = func(gtx layout.Context) layout.Dimensions {
				return option(gtx, env, optionSpec{click: &d.pick.clicks[i], label: label, selected: e.ID == d.endpoint, content: sayer(env, toneBody, label)})
			}
		}
		return flow(gtx, theme.Space2, items...)
	}
}

func (d *Diagnostics) results(env scene) []layout.Widget {
	r := d.run
	switch {
	case r.running:
		return []layout.Widget{sayer(env, toneDim, fmt.Sprintf("Running %s checks for %s…", modeWord(r.online), r.endpoint)), buttons(env, action{click: &d.cancel, label: "Cancel checks", variant: quiet})}
	case errors.Is(r.err, context.Canceled) || kindOf(r.err) == "cancelled":
		return []layout.Widget{sayer(env, toneWarning, "Checks were cancelled; no result is shown as complete.")}
	case r.err != nil:
		return []layout.Widget{sayer(env, toneFailed, "Checks failed for "+string(r.endpoint)+": "+explain(r.err))}
	case r.rows == nil:
		return nil
	}
	out := []layout.Widget{sayer(env, toneHeading, fmt.Sprintf("%s checks for %s%s", modeWord(r.online), r.endpoint, sourceNote(r.local)))}
	for _, row := range r.rows {
		out = append(out, sayer(env, rowTone(row.Status), fmt.Sprintf("%s · %s · %s", statusLabel(row.Status), row.Check, row.Detail)))
	}
	out = append(out, buttons(env,
		action{click: &d.copyBtn, label: "Copy diagnostics", variant: quiet},
		action{click: &d.export, label: "Export diagnostics", variant: quiet}))
	if d.exported != "" {
		out = append(out, sayer(env, toneAdded, d.exported))
	}
	if d.expErr != nil {
		out = append(out, sayer(env, toneFailed, "Export failed: "+d.expErr.Error()))
	}
	return out
}

func modeWord(online bool) string {
	if online {
		return "Online"
	}
	return "Local"
}

func sourceNote(local bool) string {
	if local {
		return " (run on this computer while the daemon is not connected)"
	}
	return ""
}

func rowTone(status string) tone {
	switch status {
	case "fail":
		return toneFailed
	case "warn":
		return toneWarning
	}
	return toneBody
}

func statusLabel(status string) string {
	switch status {
	case "ok":
		return "OK"
	case "warn":
		return "Warning"
	case "fail":
		return "Failed"
	}
	return status
}

func (d *Diagnostics) report() string {
	var b strings.Builder
	fmt.Fprintf(&b, "dalgona diagnostics for %s (%s)\n", d.run.endpoint, strings.ToLower(modeWord(d.run.online)))
	for _, row := range d.run.rows {
		fmt.Fprintf(&b, "%s\t%s\t%s\n", row.Status, row.Check, row.Detail)
	}
	return b.String()
}

func (d *Diagnostics) exportReport() {
	text := d.report()
	d.exported, d.expErr = "", nil
	d.work.spawn(func() func() {
		err := d.hub.export("dalgona-diagnostics.txt", []byte(text))
		return func() {
			d.expErr = err
			if err == nil {
				d.exported = "Exported the masked diagnostics."
			}
		}
	})
}
