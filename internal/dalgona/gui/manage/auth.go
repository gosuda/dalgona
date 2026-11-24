//go:build gui

package manage

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"slices"
	"time"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	flowAPIKey         = "apiKey"
	flowBrowser        = "browserCallback"
	flowManual         = "manualCode"
	flowDevice         = "deviceCode"
	flowCursor         = "cursorDeepLink"
	authPollInterval   = 2 * time.Second
	apiKeyLimitBytes   = 4096
	authCodeLimitBytes = 16384
)

var (
	apiKeyPolicy   = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: apiKeyLimitBytes}}
	authCodePolicy = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: authCodeLimitBytes}}
)

func callbackMode(flow string) string {
	switch flow {
	case flowBrowser:
		return "backendLoopback"
	case flowManual:
		return "manualPaste"
	case flowDevice:
		return "device"
	}
	return ""
}

func flowLabel(flow string) string {
	switch flow {
	case flowAPIKey:
		return "API key"
	case flowBrowser:
		return "Browser sign-in"
	case flowManual:
		return "Paste the redirect or code"
	case flowDevice:
		return "Device code"
	case flowCursor:
		return "Cursor handoff"
	}
	return flow
}

func terminalAuthState(s string) bool {
	return s == "succeeded" || s == "failed" || s == "cancelled" || s == "expired"
}

type authFlow struct {
	target     authTarget
	name       string
	desc       *authDescriptor
	loading    bool
	loadErr    error
	epoch      uint64
	status     *authStatus
	flow       string
	start      mutation
	submitOp   mutation
	logoutOp   mutation
	cancelErr  error
	pick       choice
	input      *field
	keyInput   *field
	submit     widget.Clickable
	cancel     widget.Clickable
	retry      widget.Clickable
	logout     widget.Clickable
	browse     widget.Clickable
	reload     widget.Clickable
	polling    bool
	browseErr  error
	browsedTx  string
	browsedURL string
}

func newAuthFlow() *authFlow {
	return &authFlow{input: newField(authCodePolicy, false), keyInput: newField(apiKeyPolicy, true)}
}

func (a *authFlow) bind(target authTarget, name string) {
	a.target, a.name = target, name
	a.epoch++
	a.desc, a.loading, a.loadErr, a.status, a.flow, a.polling = nil, false, nil, nil, "", false
	a.browsedTx, a.browsedURL = "", ""
	a.start.state, a.submitOp.state, a.logoutOp.state = outcomeIdle, outcomeIdle, outcomeIdle
	a.input.set("")
	a.keyInput.set("")
}

func (a *authFlow) load(h *Hub, work *tasks, b binding, thread string) {
	a.loading, a.loadErr = true, nil
	epoch := a.epoch
	work.spawn(func() func() {
		var res authProviders
		err := h.call(b, metadataDeadline, methodAuthProviders, threadOnly{ThreadID: thread}, &res)
		return func() {
			if a.epoch != epoch {
				return
			}
			a.loading, a.loadErr, a.desc = false, err, nil
			for i := range res.Targets {
				if sameTarget(res.Targets[i].Target, a.target) {
					a.desc = &res.Targets[i]
				}
			}
		}
	})
}

func sameTarget(x, y authTarget) bool {
	return x.Kind == y.Kind && x.ID == y.ID && x.Name == y.Name
}

func (a *authFlow) offered(kind client.EndpointKind) []string {
	if a.desc == nil {
		return nil
	}
	return slices.DeleteFunc(slices.Clone(a.desc.Flows), func(f string) bool { return f == flowBrowser && kind != client.EndpointLocal })
}

func (a *authFlow) active() bool {
	return a.start.running() || a.status != nil && !terminalAuthState(a.status.State)
}

type authWork struct {
	hub  *Hub
	work *tasks
	view *state.View
	bind binding
}

func (a *authFlow) update(gtx layout.Context, w authWork, kind client.EndpointKind) {
	a.input.update(gtx)
	a.keyInput.update(gtx)
	a.updateChoice(gtx, w, kind)
	a.updateRetry(gtx, w)
	a.updateSubmit(gtx, w)
	a.updateCancel(gtx, w)
	a.updateLogout(gtx, w)
	a.updateBrowse(gtx, w)
	w.hub.checkOutcome(gtx, w.work, w.view, &a.start, a.acceptReceipt(w.hub, w.work, w.bind))
	w.hub.checkOutcome(gtx, w.work, w.view, &a.submitOp, a.acceptReceipt(w.hub, w.work, w.bind))
	w.hub.checkOutcome(gtx, w.work, w.view, &a.logoutOp, func(jsontext.Value) {
		a.load(w.hub, w.work, w.bind, a.target.ThreadID)
	})
}

func (a *authFlow) acceptReceipt(h *Hub, work *tasks, b binding) func(jsontext.Value) {
	return func(raw jsontext.Value) {
		var st authStatus
		if json.Unmarshal(raw, &st) == nil {
			a.accept(h, work, b, st)
		}
	}
}

func (a *authFlow) updateChoice(gtx layout.Context, w authWork, kind client.EndpointKind) {
	flows := a.offered(kind)
	if at := a.pick.picked(gtx, len(flows)); at >= 0 && !a.active() {
		a.begin(w.hub, w.work, w.bind, flows[at])
	}
}

func (a *authFlow) updateRetry(gtx layout.Context, w authWork) {
	if clicked(gtx, &a.retry, a.active() || a.flow == "") {
		a.begin(w.hub, w.work, w.bind, a.flow)
	}
}

func (a *authFlow) updateSubmit(gtx layout.Context, w authWork) {
	if clicked(gtx, &a.submit, !a.submittable() || a.submitOp.running()) {
		a.send(w.hub, w.work, w.bind)
	}
}

func (a *authFlow) submittable() bool {
	if a.status == nil {
		return false
	}
	switch a.flow {
	case flowAPIKey:
		return a.status.State == "awaitingInput"
	case flowManual:
		return a.status.State == "awaitingInput" || a.status.State == "waiting"
	}
	return false
}

func (a *authFlow) updateCancel(gtx layout.Context, w authWork) {
	if clicked(gtx, &a.cancel, a.status == nil || terminalAuthState(a.status.State)) {
		a.abort(w.hub, w.work, w.bind)
	}
}

func (a *authFlow) updateLogout(gtx layout.Context, w authWork) {
	if clicked(gtx, &a.logout, a.desc == nil || !a.desc.SignedIn || a.logoutOp.running() || a.active()) {
		a.signOut(w.hub, w.work, w.bind)
	}
}

func (a *authFlow) updateBrowse(gtx layout.Context, w authWork) {
	if clicked(gtx, &a.browse, a.challengeURL() == "") {
		a.browseErr = w.hub.browse(a.challengeURL())
	}
}

func (h *Hub) browse(url string) error {
	if h.deps.Browse == nil {
		return fmt.Errorf("this build cannot open a browser; open %s yourself", url)
	}
	return h.deps.Browse(url)
}

func (a *authFlow) awaitingInput() bool {
	return a.status != nil && a.status.State == "awaitingInput"
}

func (a *authFlow) challengeURL() string {
	if a.status == nil || a.status.Challenge == nil || terminalAuthState(a.status.State) {
		return ""
	}
	return cmpOr(a.status.Challenge.AuthorizationURL, a.status.Challenge.VerificationURL)
}

func (a *authFlow) begin(h *Hub, work *tasks, b binding, flow string) {
	a.flow, a.status, a.browseErr, a.cancelErr = flow, nil, nil, nil
	id := a.start.begin(methodAuthStart, b, true)
	params := authStartParams{Target: a.target, Flow: flow, CallbackMode: callbackMode(flow), OperationID: id}
	work.spawn(func() func() {
		var st authStatus
		err := h.call(b, metadataDeadline, methodAuthStart, params, &st)
		return func() {
			if a.start.id != id {
				return
			}
			a.start.settle(err)
			if err == nil {
				a.accept(h, work, b, st)
			}
		}
	})
}

func (a *authFlow) accept(h *Hub, work *tasks, b binding, st authStatus) {
	a.status = &st
	if a.flow == flowBrowser && st.Challenge != nil && st.Challenge.AuthorizationURL != "" {
		if a.browsedTx != st.TransactionID || a.browsedURL != st.Challenge.AuthorizationURL {
			a.browsedTx, a.browsedURL = st.TransactionID, st.Challenge.AuthorizationURL
			a.browseErr = h.browse(st.Challenge.AuthorizationURL)
		}
	}
	if terminalAuthState(st.State) {
		a.finished(h, work, b)
		return
	}
	if st.State != "awaitingInput" && !a.polling {
		a.poll(h, work, b, st.TransactionID)
	}
}

func (a *authFlow) poll(h *Hub, work *tasks, b binding, transaction string) {
	a.polling = true
	epoch := a.epoch
	work.spawn(func() func() {
		timer := time.NewTimer(authPollInterval)
		defer timer.Stop()
		select {
		case <-timer.C:
		case <-h.ctx.Done():
			return nil
		}
		var st authStatus
		err := h.call(b, metadataDeadline, methodAuthStatus, authTransaction{TransactionID: transaction}, &st)
		return func() {
			if a.epoch != epoch || a.status == nil || a.status.TransactionID != transaction {
				return
			}
			a.polling = false
			if err != nil {
				a.start.state, a.start.err = outcomeFailed, err
				return
			}
			a.accept(h, work, b, st)
		}
	})
}

func (a *authFlow) finished(h *Hub, work *tasks, b binding) {
	a.input.set("")
	a.keyInput.set("")
	if a.status.State == "succeeded" {
		a.load(h, work, b, a.target.ThreadID)
	}
}

func (a *authFlow) send(h *Hub, work *tasks, b binding) {
	response := authResponse{Kind: "authorizationCode", Value: a.input.text()}
	switch {
	case a.flow == flowAPIKey:
		response = authResponse{Kind: "apiKey", Value: a.keyInput.text()}
	case looksLikeURL(a.input.text()):
		response.Kind = "redirectUrl"
	}
	if inputtext.Blank(response.Value) {
		a.submitOp.state, a.submitOp.err = outcomeFailed, fmt.Errorf("enter the %s first", flowLabel(a.flow))
		return
	}
	id := a.submitOp.begin(methodAuthSubmit, b, true)
	transaction := a.status.TransactionID
	params := authSubmitParams{TransactionID: transaction, Response: response, OperationID: id}
	work.spawn(func() func() {
		var st authStatus
		err := h.call(b, metadataDeadline, methodAuthSubmit, params, &st)
		return func() {
			if a.submitOp.id != id {
				return
			}
			a.submitOp.settle(err)
			if err == nil {
				a.accept(h, work, b, st)
			}
		}
	})
}

func looksLikeURL(s string) bool {
	return len(s) > 8 && (s[:7] == "http://" || s[:8] == "https://")
}

func (a *authFlow) abort(h *Hub, work *tasks, b binding) {
	transaction := a.status.TransactionID
	work.spawn(func() func() {
		var res struct {
			Status string `json:"status"`
		}
		err := h.call(b, metadataDeadline, methodAuthCancel, authTransaction{TransactionID: transaction}, &res)
		return func() {
			if a.status == nil || a.status.TransactionID != transaction {
				return
			}
			a.cancelErr = err
			if err != nil {
				return
			}
			switch res.Status {
			case "completed":
				a.resolveCancelled(h, work, b, transaction)
			case "notFound":
				a.status.State = "cancelled"
				a.finished(h, work, b)
			default:
				a.status.State = "cancelled"
				a.finished(h, work, b)
			}
		}
	})
}

func (a *authFlow) resolveCancelled(h *Hub, work *tasks, b binding, transaction string) {
	work.spawn(func() func() {
		var st authStatus
		err := h.call(b, metadataDeadline, methodAuthStatus, authTransaction{TransactionID: transaction}, &st)
		return func() {
			if a.status == nil || a.status.TransactionID != transaction {
				return
			}
			if err != nil {
				a.cancelErr = err
				return
			}
			a.accept(h, work, b, st)
		}
	})
}

func (a *authFlow) signOut(h *Hub, work *tasks, b binding) {
	id := a.logoutOp.begin(methodAuthLogout, b, true)
	params := authLogoutParams{Target: a.target, OperationID: id}
	work.spawn(func() func() {
		err := h.call(b, metadataDeadline, methodAuthLogout, params, nil)
		return func() {
			if a.logoutOp.id != id {
				return
			}
			a.logoutOp.settle(err)
			if err == nil {
				a.load(h, work, b, a.target.ThreadID)
			}
		}
	})
}

func (a *authFlow) widgets(env scene, kind client.EndpointKind) []layout.Widget {
	switch {
	case a.loadErr != nil:
		return []layout.Widget{sayer(env, toneFailed, "Sign-in methods did not load: "+explain(a.loadErr)), buttons(env, action{click: &a.reload, label: "Load sign-in methods", variant: secondary})}
	case a.loading:
		return []layout.Widget{sayer(env, toneDim, "Loading sign-in methods…")}
	case a.desc == nil:
		return []layout.Widget{sayer(env, toneWarning, "The daemon offers no sign-in target for "+a.name+".")}
	case a.desc.UnavailableReason != "":
		return []layout.Widget{sayer(env, toneWarning, "Sign-in is unavailable: "+a.desc.UnavailableReason)}
	}
	out := []layout.Widget{sayer(env, toneBody, a.account())}
	out = append(out, a.flowChoices(env, kind))
	out = append(out, a.progress(env)...)
	return append(out, a.start.line(env), a.submitOp.line(env), a.logoutOp.line(env), buttons(env, a.actions()...))
}

func (a *authFlow) account() string {
	if !a.desc.SignedIn {
		return a.desc.DisplayName + ": not signed in."
	}
	return a.desc.DisplayName + ": signed in as " + cmpOr(a.desc.AccountLabel, "an unnamed account") + "."
}

func (a *authFlow) actions() []action {
	out := []action{}
	if a.status != nil && !terminalAuthState(a.status.State) {
		out = append(out, action{click: &a.cancel, label: "Cancel sign-in", variant: quiet})
	}
	if a.flow != "" && !a.active() {
		out = append(out, action{click: &a.retry, label: "Retry " + flowLabel(a.flow), variant: secondary})
	}
	if a.desc.SignedIn {
		out = append(out, action{click: &a.logout, label: "Sign out of " + a.desc.DisplayName, variant: destructive, off: a.logoutOp.running() || a.active()})
	}
	return out
}

func (a *authFlow) flowChoices(env scene, kind client.EndpointKind) layout.Widget {
	flows := a.offered(kind)
	a.pick.grow(len(flows))
	return func(gtx layout.Context) layout.Dimensions {
		items := make([]layout.Widget, len(flows))
		for i, f := range flows {
			items[i] = func(gtx layout.Context) layout.Dimensions {
				return button(gtx, env, action{click: &a.pick.clicks[i], label: "Sign in with " + flowLabel(f), variant: secondary, off: a.active()})
			}
		}
		return flow(gtx, theme.Space3, items...)
	}
}

func (a *authFlow) progress(env scene) []layout.Widget {
	if a.status == nil {
		return nil
	}
	out := []layout.Widget{sayer(env, authTone(a.status.State), authStateText(*a.status))}
	if c := a.status.Challenge; c != nil && !terminalAuthState(a.status.State) {
		out = append(out, challengeWidgets(env, c, &a.browse)...)
	}
	if a.browseErr != nil {
		out = append(out, sayer(env, toneWarning, "The browser did not open: "+a.browseErr.Error()))
	}
	if a.cancelErr != nil {
		out = append(out, sayer(env, toneFailed, "Cancel failed: "+explain(a.cancelErr)))
	}
	if a.awaitingInput() || a.flow == flowManual && !terminalAuthState(a.status.State) {
		out = append(out, a.inputWidgets(env)...)
	}
	return out
}

func (a *authFlow) inputWidgets(env scene) []layout.Widget {
	f, label := a.input, "Redirect URL or authorization code"
	if a.flow == flowAPIKey {
		f, label = a.keyInput, "API key"
	}
	return []layout.Widget{
		func(gtx layout.Context) layout.Dimensions { return f.layout(gtx, env, label, a.submitOp.running()) },
		buttons(env, action{click: &a.submit, label: "Submit " + label, variant: pointed, off: a.submitOp.running()}),
	}
}

func challengeWidgets(env scene, c *authChallenge, browse *widget.Clickable) []layout.Widget {
	out := []layout.Widget{}
	if c.Prompt != "" {
		out = append(out, sayer(env, toneBody, c.Prompt))
	}
	if c.UserCode != "" {
		out = append(out, sayer(env, toneCode, "Code: "+c.UserCode))
	}
	if url := cmpOr(c.AuthorizationURL, c.VerificationURL); url != "" {
		out = append(out, sayer(env, toneCode, url), buttons(env, action{click: browse, label: "Open in browser", variant: secondary}))
	}
	if c.ExpiresAt != "" {
		out = append(out, sayer(env, toneDim, "Expires at "+c.ExpiresAt))
	}
	return out
}

func authTone(state string) tone {
	switch state {
	case "failed", "expired":
		return toneFailed
	case "succeeded":
		return toneAdded
	}
	return toneDim
}

func authStateText(st authStatus) string {
	switch st.State {
	case "awaitingInput":
		return "Waiting for your input."
	case "waiting":
		return "Waiting for the sign-in to complete…"
	case "exchanging":
		return "Exchanging the authorization…"
	case "succeeded":
		return "Signed in."
	case "cancelled":
		return "Sign-in cancelled; no credential changed."
	case "expired":
		return "The sign-in expired; retry to start a fresh one."
	case "failed":
		return "Sign-in failed: " + authError(st)
	}
	return st.State
}

func authError(st authStatus) string {
	var e rpc.RPCError
	if len(st.Error) > 0 && json.Unmarshal(st.Error, &e) == nil && e.Message != "" {
		return e.Message
	}
	var s string
	if json.Unmarshal(st.Error, &s) == nil && s != "" {
		return s
	}
	return "the daemon gave no reason"
}

func (a *authFlow) reloadClicked(gtx layout.Context) bool { return clicked(gtx, &a.reload, a.loading) }
