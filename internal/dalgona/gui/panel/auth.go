//go:build gui

package panel

import (
	"strings"
	"time"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	flowAPIKey    = "apiKey"
	flowBrowser   = "browserCallback"
	flowManual    = "manualCode"
	flowDevice    = "deviceCode"
	flowCursor    = "cursorDeepLink"
	apiKeyLimit   = 4096
	pastedLimit   = 16384
	pollEvery     = time.Second
	authSucceeded = "succeeded"
	authFailed    = "failed"
	authCancelled = "cancelled"
	authExpired   = "expired"
	authAwaiting  = "awaitingInput"
)

var (
	callbackModes = map[string]string{flowBrowser: "backendLoopback", flowManual: "manualPaste", flowDevice: "device"}
	flowLabels    = map[string]string{
		flowAPIKey: "API key", flowBrowser: "Sign in with the browser", flowManual: "Sign in and paste the code",
		flowDevice: "Device code", flowCursor: "Cursor app",
	}
	keyPolicy   = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: apiKeyLimit}}
	pastePolicy = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: pastedLimit}}
)

type authTarget struct {
	Kind     string `json:"kind"`
	ID       string `json:"id,omitzero"`
	Name     string `json:"name,omitzero"`
	ThreadID string `json:"threadId,omitzero"`
}

type authDescriptor struct {
	Target            authTarget `json:"target"`
	DisplayName       string     `json:"displayName"`
	SignedIn          bool       `json:"signedIn"`
	AccountLabel      string     `json:"accountLabel"`
	Flows             []string   `json:"flows"`
	UnavailableReason string     `json:"unavailableReason"`
}

type authChallenge struct {
	Kind             string `json:"kind"`
	AuthorizationURL string `json:"authorizationUrl"`
	VerificationURL  string `json:"verificationUrl"`
	UserCode         string `json:"userCode"`
	Prompt           string `json:"prompt"`
	ExpiresAt        string `json:"expiresAt"`
}

type authStatus struct {
	TransactionID string         `json:"transactionId"`
	Target        authTarget     `json:"target"`
	State         string         `json:"state"`
	Challenge     *authChallenge `json:"challenge"`
	Error         string         `json:"error"`
	ExpiresAt     string         `json:"expiresAt"`
}

type authStartParams struct {
	Target       authTarget `json:"target"`
	Flow         string     `json:"flow"`
	CallbackMode string     `json:"callbackMode,omitzero"`
}

type authResponse struct {
	Kind  string `json:"kind"`
	Value string `json:"value"`
}

type authSubmitParams struct {
	TransactionID string       `json:"transactionId"`
	Response      authResponse `json:"response"`
}

type authTransactionParams struct {
	TransactionID string `json:"transactionId"`
}

type authTargetParams struct {
	Target authTarget `json:"target"`
}

type authProvidersParams struct {
	ThreadID string `json:"threadId,omitzero"`
}

func liveAuth(state string) bool {
	return state == authAwaiting || state == "waiting" || state == "exchanging"
}

type authFlow struct {
	endpoint   client.EndpointID
	target     authTarget
	flow       string
	key        string
	start      pending
	submit     pending
	poll       pending
	id         string
	generation client.Generation
	status     authStatus
	began      bool
	ended      bool
	uncertain  bool
	fault      string
	nextPoll   time.Time
}

func (f *authFlow) begin(s *scene, endpoint client.EndpointID, target authTarget, flow string) {
	*f = authFlow{endpoint: endpoint, target: target, flow: flow, began: true}
	f.start.start(s, request{
		endpoint: endpoint, method: "dalgona/auth/start", operation: true,
		params: authStartParams{Target: target, Flow: flow, CallbackMode: callbackModes[flow]},
	})
}

func (f *authFlow) live() bool {
	return f.began && !f.ended
}

func (f *authFlow) update(gtx layout.Context, s *scene) {
	f.settleStart(s)
	f.settleSubmit(s)
	f.settlePoll(s)
	if !f.live() || f.id == "" || f.status.State == authAwaiting {
		return
	}
	if !f.poll.running() && !gtx.Now.Before(f.nextPoll) {
		f.poll.start(s, request{endpoint: f.endpoint, method: "dalgona/auth/status", params: authTransactionParams{TransactionID: f.id}})
	}
	if f.nextPoll.IsZero() || !gtx.Now.Before(f.nextPoll) {
		f.nextPoll = gtx.Now.Add(pollEvery)
	}
	gtx.Execute(op.InvalidateCmd{At: f.nextPoll})
}

func (f *authFlow) settleStart(s *scene) {
	if f.start.unknown(s) {
		f.uncertain = true
	}
	v, done := f.start.settle(s)
	if !done {
		return
	}
	status, err := decodeCall[authStatus](v)
	if err != nil {
		f.ended, f.fault = true, err.Error()
		return
	}
	f.id, f.generation = status.TransactionID, v.Generation
	f.apply(status)
	if f.flow == flowAPIKey && status.State == authAwaiting && f.key != "" {
		f.send(s, authResponse{Kind: flowAPIKey, Value: f.key})
		f.key = ""
	}
}

func (f *authFlow) send(s *scene, response authResponse) {
	f.fault = ""
	f.submit.start(s, request{
		endpoint: f.endpoint, method: "dalgona/auth/submit", operation: true,
		params: authSubmitParams{TransactionID: f.id, Response: response},
	})
}

func (f *authFlow) settleSubmit(s *scene) {
	if f.submit.unknown(s) {
		f.uncertain = true
	}
	v, done := f.submit.settle(s)
	if !done {
		return
	}
	status, err := decodeCall[authStatus](v)
	if err != nil {
		f.fault = err.Error()
		if data, coded := rpc.ErrorDataOf(err); coded && data.Kind == rpc.ErrorExpired {
			f.ended = true
		}
		return
	}
	f.apply(status)
}

func (f *authFlow) settlePoll(s *scene) {
	v, done := f.poll.settle(s)
	if !done || f.ended {
		return
	}
	if v.Endpoint != f.endpoint || v.Generation != f.generation {
		f.ended, f.fault = true, "The connection to the endpoint changed, so this sign-in ended. Start it again."
		return
	}
	status, err := decodeCall[authStatus](v)
	if err != nil {
		f.ended, f.fault = true, err.Error()
		return
	}
	if status.TransactionID == f.id {
		f.apply(status)
	}
}

func (f *authFlow) apply(status authStatus) {
	f.status = status
	if !liveAuth(status.State) {
		f.ended = true
	}
}

func (f *authFlow) succeeded() bool {
	return f.ended && f.status.State == authSucceeded
}

func (f *authFlow) abandon(out *[]state.Intent, orphans *[]orphan) {
	f.submit.release(out)
	f.poll.release(out)
	switch {
	case f.start.running():
		*orphans = append(*orphans, orphan{call: f.start, endpoint: f.endpoint, start: true})
		f.start = pending{}
	case f.live() && f.id != "":
		*orphans = append(*orphans, cancelTransaction(out, f.endpoint, f.id))
	}
	f.ended, f.key = true, ""
}

type orphan struct {
	call     pending
	endpoint client.EndpointID
	start    bool
}

func cancelTransaction(out *[]state.Intent, endpoint client.EndpointID, id string) orphan {
	call := pending{id: mint()}
	*out = append(*out, state.Call{ID: call.id, Endpoint: endpoint, Method: "dalgona/auth/cancel", Params: authTransactionParams{TransactionID: id}})
	return orphan{call: call, endpoint: endpoint}
}

func settleOrphans(s *scene, orphans []orphan) []orphan {
	kept := orphans[:0]
	var follow []orphan
	for _, o := range orphans {
		if o.call.unknown(s) {
			kept = append(kept, o)
			continue
		}
		v, done := o.call.settle(s)
		if !done {
			kept = append(kept, o)
			continue
		}
		if status, err := decodeCall[authStatus](v); o.start && err == nil && liveAuth(status.State) {
			follow = append(follow, cancelTransaction(s.out, o.endpoint, status.TransactionID))
		}
	}
	return append(kept, follow...)
}

func expiry(at string) string {
	t, err := time.Parse(time.RFC3339, at)
	if err != nil {
		return ""
	}
	return "Expires at " + t.Local().Format("15:04") + "."
}

func pasteKind(value string) string {
	if strings.Contains(value, "://") {
		return "redirectUrl"
	}
	return "authorizationCode"
}
