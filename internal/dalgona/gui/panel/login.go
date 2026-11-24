//go:build gui

package panel

import (
	"context"
	"slices"
	"time"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const openTimeout = 30 * time.Second

var openURL = platform.OpenURL

type providersReply struct {
	Targets []authDescriptor `json:"targets"`
}

type loginPanel struct {
	endpoint    client.EndpointID
	thread      string
	picker      endpointPicker
	list        pending
	targets     []authDescriptor
	loaded      bool
	listErr     string
	recheck     bool
	retry       Button
	choices     []Choice
	picked      string
	flowChoices []Choice
	flowName    string
	key         field
	paste       field
	flow        authFlow
	handled     bool
	begin       Button
	cancel      Button
	send        Button
	copyLink    Button
	copyCode    Button
	openLink    Button
	logout      operation
	done        string
	orphans     []orphan
	opened      chan error
	opening     bool
	openErr     string
}

func targetKey(t authTarget) string {
	return t.Kind + "\x00" + t.ID + "\x00" + t.Name
}

func (l *loginPanel) open(r shell.Route) {
	orphans, picker := l.orphans, l.picker
	*l = loginPanel{endpoint: r.Thread.Endpoint, thread: r.Thread.Thread, orphans: orphans, picker: picker, opened: make(chan error, 1)}
	l.key = newField("API key", keyPolicy)
	l.key.secret = true
	l.paste = newField("Redirect URL or authorization code", pastePolicy)
	l.paste.secret = true
}

func (l *loginPanel) release(out *[]state.Intent) {
	l.flow.abandon(out, &l.orphans)
	l.list.release(out)
	l.logout.call.release(out)
	l.key.clear()
	l.paste.clear()
}

func (l *loginPanel) settle(s *scene) {
	l.orphans = settleOrphans(s, l.orphans)
}

func (l *loginPanel) reload(s *scene) {
	l.listErr = ""
	l.list.start(s, request{endpoint: l.endpoint, method: "dalgona/auth/providers", params: authProvidersParams{ThreadID: l.thread}})
}

func (l *loginPanel) settleList(s *scene) {
	v, done := l.list.settle(s)
	if !done {
		return
	}
	reply, err := decodeCall[providersReply](v)
	if err != nil {
		l.listErr = err.Error()
		return
	}
	l.targets, l.loaded, l.recheck = reply.Targets, true, false
}

func (l *loginPanel) chosen() (authDescriptor, bool) {
	at := slices.IndexFunc(l.targets, func(d authDescriptor) bool { return targetKey(d.Target) == l.picked })
	if at < 0 {
		return authDescriptor{}, false
	}
	return l.targets[at], true
}

func (l *loginPanel) local(v *state.View) bool {
	ep, _ := endpointView(v, l.endpoint)
	return ep.Kind == client.EndpointLocal
}

func (l *loginPanel) offered(v *state.View, d authDescriptor) []string {
	local := l.local(v)
	return slices.DeleteFunc(slices.Clone(d.Flows), func(flow string) bool {
		_, known := flowLabels[flow]
		return !known || flow == flowBrowser && !local
	})
}

func (l *loginPanel) currentFlow(v *state.View, d authDescriptor) string {
	flows := l.offered(v, d)
	if slices.Contains(flows, l.flowName) {
		return l.flowName
	}
	if len(flows) > 0 {
		return flows[0]
	}
	return ""
}

func (l *loginPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	l.update(gtx, s)
	content := l.picker.rows(s, "Endpoint", l.endpoint)
	content = append(content, l.providerRows(s)...)
	if d, ok := l.chosen(); ok {
		content = append(content, l.detailRows(s, d)...)
	}
	n, row := rows(content)
	return s.show(gtx, Content{Title: titles["login"], Rows: n, Row: row})
}

func (l *loginPanel) update(gtx layout.Context, s *scene) {
	l.settle(s)
	l.endpoint = defaultEndpoint(s.env, l.endpoint)
	if id, changed := l.picker.update(gtx, s.env.View.Endpoints(), l.endpoint); changed {
		l.release(s.out)
		l.open(shell.Route{Thread: state.ThreadKey{Endpoint: id}})
	}
	if l.retry.Clicked(gtx) || !l.loaded && !l.list.running() && l.listErr == "" {
		l.reload(s)
	}
	l.settleList(s)
	l.flow.update(gtx, s)
	l.finish(s)
	l.pick(gtx, s)
	l.fields(gtx, s)
	l.buttons(gtx, s)
	l.links(gtx)
}

func (l *loginPanel) finish(s *scene) {
	if l.flow.uncertain && !l.recheck {
		l.recheck = true
		l.reload(s)
	}
	if !l.flow.ended || l.handled {
		return
	}
	l.handled = true
	d, _ := l.chosen()
	switch l.flow.status.State {
	case authSucceeded:
		l.done = "Logged in to " + d.DisplayName + "."
		l.key.clear()
		l.paste.clear()
		s.announce(platform.AnnounceCompletion, l.done)
		s.env.Submit(state.RefreshEndpoint{Endpoint: l.endpoint})
		l.reload(s)
	case authCancelled, authExpired:
		l.key.clear()
		l.paste.clear()
	}
}

func (l *loginPanel) pick(gtx layout.Context, s *scene) {
	for i := range min(len(l.choices), len(l.targets)) {
		key := targetKey(l.targets[i].Target)
		if l.choices[i].Clicked(gtx) && key != l.picked {
			l.flow.abandon(s.out, &l.orphans)
			l.picked, l.flowName, l.done, l.logout.err = key, "", "", ""
			l.key.clear()
			l.paste.clear()
		}
	}
	d, ok := l.chosen()
	if !ok {
		return
	}
	flows := l.offered(s.env.View, d)
	for i := range min(len(l.flowChoices), len(flows)) {
		if l.flowChoices[i].Clicked(gtx) && !l.flow.live() {
			l.flowName = flows[i]
		}
	}
}

func (l *loginPanel) fields(gtx layout.Context, s *scene) {
	key := l.key.update(gtx)
	paste := l.paste.update(gtx)
	if key.cancel || paste.cancel {
		s.close()
		return
	}
	if key.submit {
		l.login(s)
	}
	if paste.submit {
		l.sendPaste(s)
	}
}

func (l *loginPanel) buttons(gtx layout.Context, s *scene) {
	if l.begin.Clicked(gtx) {
		l.login(s)
	}
	if l.send.Clicked(gtx) {
		l.sendPaste(s)
	}
	if l.cancel.Clicked(gtx) && l.flow.live() {
		l.flow.abandon(s.out, &l.orphans)
		l.flow.status.State, l.handled = authCancelled, false
	}
	if l.logout.run.Clicked(gtx) && !l.logout.call.running() {
		l.signOut(s)
	}
	if _, ok := l.logout.settle(s); ok {
		d, _ := l.chosen()
		l.done = "Logged out of " + d.DisplayName + ". Models from it are no longer available; your drafts are kept."
		s.announce(platform.AnnounceCompletion, l.done)
		s.env.Submit(state.RefreshEndpoint{Endpoint: l.endpoint})
		l.reload(s)
	}
}

func (l *loginPanel) blocked() bool {
	return l.recheck || l.flow.start.running() || l.flow.submit.running()
}

func (l *loginPanel) login(s *scene) {
	d, ok := l.chosen()
	if !ok || l.blocked() {
		return
	}
	flow := l.currentFlow(s.env.View, d)
	if flow != flowAPIKey {
		if !l.flow.live() {
			l.start(s, d, flow)
		}
		return
	}
	key := l.key.text()
	if problem := keyProblem(key); problem != "" {
		l.key.fault = problem
		return
	}
	if l.flow.live() && l.flow.flow == flowAPIKey && l.flow.status.State == authAwaiting {
		l.flow.send(s, authResponse{Kind: flowAPIKey, Value: key})
		return
	}
	l.start(s, d, flowAPIKey)
	l.flow.key = key
}

func keyProblem(key string) string {
	if err := keyPolicy.Check(key); err != nil {
		return faultNotice(err, keyPolicy)
	}
	if inputtext.Blank(key) {
		return "Enter the API key."
	}
	return ""
}

func (l *loginPanel) start(s *scene, d authDescriptor, flow string) {
	l.done, l.handled, l.openErr = "", false, ""
	l.flow.begin(s, l.endpoint, d.Target, flow)
}

func (l *loginPanel) sendPaste(s *scene) {
	value := l.paste.text()
	if !l.flow.live() || l.flow.flow != flowManual || l.flow.id == "" || l.blocked() {
		return
	}
	if err := pastePolicy.Check(value); err != nil {
		l.paste.fault = faultNotice(err, pastePolicy)
		return
	}
	if inputtext.Blank(value) {
		l.paste.fault = "Paste the redirect URL or the authorization code."
		return
	}
	l.flow.send(s, authResponse{Kind: pasteKind(value), Value: value})
}

func (l *loginPanel) signOut(s *scene) {
	d, ok := l.chosen()
	if !ok {
		return
	}
	l.done = ""
	l.logout.failed = "Unable to log out: "
	l.logout.start(s, request{endpoint: l.endpoint, method: "dalgona/auth/logout", params: authTargetParams{Target: d.Target}})
}

func (l *loginPanel) links(gtx layout.Context) {
	c := l.flow.status.Challenge
	if c == nil {
		return
	}
	link := c.AuthorizationURL
	if link == "" {
		link = c.VerificationURL
	}
	if l.copyLink.Clicked(gtx) {
		l.copied(platform.WriteText(gtx.Source, link))
	}
	if l.copyCode.Clicked(gtx) {
		l.copied(platform.WriteText(gtx.Source, c.UserCode))
	}
	if l.openLink.Clicked(gtx) && !l.opening {
		l.launch(link)
	}
	l.settleOpen()
}

func (l *loginPanel) settleOpen() {
	select {
	case err := <-l.opened:
		l.opening = false
		if err != nil {
			l.openErr = "Unable to open the link: " + err.Error() + ". Copy it into a browser instead."
		}
	default:
	}
}

func (l *loginPanel) copied(err error) {
	l.openErr = ""
	if err != nil {
		l.openErr = "Unable to copy: " + err.Error()
	}
}

func (l *loginPanel) launch(link string) {
	l.opening, l.openErr = true, ""
	results := l.opened
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), openTimeout)
		defer cancel()
		results <- openURL(ctx, link, "https", "cursor")
	}()
}
