//go:build gui

package panel

import (
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const folderTitle = "Choose a working directory"

type newThreadPanel struct {
	dialogs   Dialogs
	endpoint  client.EndpointID
	picker    endpointPicker
	path      field
	folder    string
	chooseErr string
	choose    Button
	op        operation
}

func (n *newThreadPanel) open(r shell.Route) {
	*n = newThreadPanel{dialogs: n.dialogs, picker: n.picker, endpoint: r.Thread.Endpoint}
	n.path = newField("Working directory path", linePolicy)
}

func (n *newThreadPanel) release(out *[]state.Intent) { n.op.call.release(out) }

func (n *newThreadPanel) chosen(result platform.DialogResult, err error) {
	switch {
	case err != nil:
		n.chooseErr = "Unable to choose a folder: " + err.Error()
	case !result.Cancelled && len(result.Files) > 0:
		n.folder, n.chooseErr = result.Files[0], ""
	}
}

func (n *newThreadPanel) local(v *state.View) bool {
	ep, _ := endpointView(v, n.endpoint)
	return ep.Kind == client.EndpointLocal
}

func (n *newThreadPanel) directory(v *state.View) string {
	if n.local(v) {
		return n.folder
	}
	return n.path.text()
}

func (n *newThreadPanel) update(gtx layout.Context, s *scene) {
	n.endpoint = defaultEndpoint(s.env, n.endpoint)
	if id, changed := n.picker.update(gtx, s.env.View.Endpoints(), n.endpoint); changed && !n.op.call.running() {
		n.endpoint, n.op.err = id, ""
	}
	ev := n.path.update(gtx)
	if ev.cancel {
		s.close()
		return
	}
	if n.choose.Clicked(gtx) {
		n.chooseFolder()
	}
	if (n.op.run.Clicked(gtx) || ev.submit) && !n.op.call.running() {
		n.start(s)
	}
	if v, ok := n.op.settle(s); ok {
		n.started(s, v)
	}
}

func (n *newThreadPanel) chooseFolder() {
	if n.dialogs == nil {
		return
	}
	n.chooseErr = ""
	if err := n.dialogs.ShowDialog(platform.DialogRequest{Kind: platform.DialogChooseFolder, Title: folderTitle}); err != nil {
		n.chooseErr = "Unable to open the folder chooser: " + err.Error()
	}
}

func (n *newThreadPanel) start(s *scene) {
	dir := n.directory(s.env.View)
	if err := linePolicy.Check(dir); err != nil {
		n.op.err = faultNotice(err, linePolicy)
		return
	}
	ep, _ := endpointView(s.env.View, n.endpoint)
	params := rpc.ThreadStartParams{Model: ep.Config.Model, Cwd: dir}
	if ep.Config.ModelReasoningEffort != "" {
		params.Config = &rpc.ThreadStartConfig{ModelReasoningEffort: ep.Config.ModelReasoningEffort}
	}
	n.op.failed = "Unable to start the thread on " + endpointName(s.env.View, n.endpoint) + ": "
	n.op.start(s, request{endpoint: n.endpoint, method: "thread/start", params: params})
}

func (n *newThreadPanel) started(s *scene, v state.CallView) {
	started, err := decode[rpc.ThreadStartResponse](v)
	if err != nil || started.Thread.ID == "" {
		n.op.err = "The daemon started a thread without naming it; find it in the thread list of " + endpointName(s.env.View, n.endpoint) + "."
		return
	}
	s.announce(platform.AnnounceCompletion, "Thread started in "+started.Cwd)
	s.close()
	if s.env.Navigate != nil {
		s.env.Navigate(state.ThreadKey{Endpoint: v.Endpoint, Thread: started.Thread.ID})
	}
}

func (n *newThreadPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	n.update(gtx, s)
	content := n.picker.choices(s, "Run on", n.endpoint)
	content = append(content, n.facts(s)...)
	content = append(content, n.directoryRows(s)...)
	content = append(content, n.op.status(s, "Starting the thread")...)
	label := "Start thread"
	if n.op.err != "" {
		label = "Try again"
	}
	rowsN, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["new"], Rows: rowsN, Row: row, Dismiss: "Cancel",
		Actions: []layout.Widget{n.op.button(s, label, "Starting", Point)},
	})
}

func (n *newThreadPanel) facts(s *scene) []layout.Widget {
	ep, _ := endpointView(s.env.View, n.endpoint)
	model := "Model: the daemon's default"
	if ep.Config.Model != "" {
		model = "Model: " + ep.Config.Model
		if ep.Config.ModelReasoningEffort != "" {
			model += " · effort " + ep.Config.ModelReasoningEffort
		}
	}
	out := []layout.Widget{s.para(model)}
	if ep.Config.ApprovalPolicy != "" {
		out = append(out, s.note("Approval: "+ep.Config.ApprovalPolicy))
	}
	if ep.Config.SandboxMode != "" {
		out = append(out, s.note("Sandbox: "+ep.Config.SandboxMode))
	}
	return append(out, s.note("Runner: chosen by "+endpointName(s.env.View, n.endpoint)+" when the thread starts"))
}

func (n *newThreadPanel) directoryRows(s *scene) []layout.Widget {
	if !n.local(s.env.View) {
		return []layout.Widget{
			func(gtx layout.Context) layout.Dimensions { return n.path.layout(gtx, s) },
			s.note("Enter a path on " + endpointName(s.env.View, n.endpoint) + " in its own path syntax. Leave it empty for the daemon's default directory."),
		}
	}
	shown := "Working directory: the daemon's default"
	if n.folder != "" {
		shown = "Working directory: " + n.folder
	}
	out := []layout.Widget{s.para(shown)}
	if n.dialogs == nil {
		return append(out, s.note("This window has no folder chooser; the daemon's default directory is used."))
	}
	out = append(out, func(gtx layout.Context) layout.Dimensions {
		return n.choose.Layout(gtx, s.frame, s.env, "Choose folder", Secondary)
	})
	if n.chooseErr != "" {
		out = append(out, s.alert(n.chooseErr))
	}
	return out
}
