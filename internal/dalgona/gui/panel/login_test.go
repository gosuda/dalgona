//go:build gui

package panel

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

var (
	openaiTarget = authTarget{Kind: "provider", ID: "openai"}
	chatTarget   = authTarget{Kind: "provider", ID: "chatgpt"}
)

func providers() providersReply {
	return providersReply{Targets: []authDescriptor{
		{Target: openaiTarget, DisplayName: "OpenAI", Flows: []string{flowAPIKey}},
		{Target: chatTarget, DisplayName: "ChatGPT", SignedIn: true, AccountLabel: "me@example.com", Flows: []string{flowBrowser, flowManual, flowDevice}},
	}}
}

func loginOn(t *testing.T, ep state.EndpointView) *harness {
	t.Helper()
	h := newHarness(t, ep)
	h.open(shell.Route{Panel: "login", Thread: state.ThreadKey{Endpoint: ep.ID}})
	h.answer(h.call("dalgona/auth/providers"), providers())
	h.take()
	return h
}

func TestAPIKeyFlowStartsWithoutCallbackAndSubmitsTheExactKey(t *testing.T) {
	h := loginOn(t, endpointOf(local, "local", client.PhaseConnected))
	h.choose("OpenAI Not signed in")
	h.typeInto(&h.panels.login.key, "  sk-test\u00a0 ")
	h.click("Log in")
	start := h.call("dalgona/auth/start")
	raw := rawParams(t, start)
	if _, has := raw["callbackMode"]; has || raw["flow"] != flowAPIKey || !start.Operation {
		t.Fatalf("API-key start params %v", raw)
	}
	h.answer(start, authStatus{TransactionID: "tx-1", Target: openaiTarget, State: authAwaiting})
	submit := params[authSubmitParams](t, h.call("dalgona/auth/submit"))
	if submit.TransactionID != "tx-1" || submit.Response != (authResponse{Kind: flowAPIKey, Value: "  sk-test\u00a0 "}) {
		t.Fatalf("submitted %+v, want the key unchanged", submit)
	}
}

func TestAPIKeyFailureStaysBesideTheFieldAndSuccessClearsTheSecret(t *testing.T) {
	h := loginOn(t, endpointOf(local, "local", client.PhaseConnected))
	h.choose("OpenAI Not signed in")
	h.typeInto(&h.panels.login.key, "sk-bad")
	h.click("Log in")
	h.answer(h.call("dalgona/auth/start"), authStatus{TransactionID: "tx-1", State: authAwaiting})
	h.fail(h.call("dalgona/auth/submit"), "the key was rejected")
	h.want("the key was rejected")
	if h.closed != 0 || h.panels.login.key.text() != "sk-bad" {
		t.Fatalf("API-key failure closed the panel or lost the field %q", h.panels.login.key.text())
	}
	h.click("Log in")
	h.answer(h.call("dalgona/auth/submit"), authStatus{TransactionID: "tx-1", State: authSucceeded})
	h.want("Logged in to OpenAI.")
	if h.panels.login.key.text() != "" {
		t.Fatal("the secret field kept the key after the flow ended")
	}
	if refresh := submitted[state.RefreshEndpoint](h.intents); len(refresh) != 1 {
		t.Fatal("success did not refresh account state and models")
	}
}

func TestEmptyAPIKeyIsRefusedLocally(t *testing.T) {
	h := loginOn(t, endpointOf(local, "local", client.PhaseConnected))
	h.choose("OpenAI Not signed in")
	h.typeInto(&h.panels.login.key, " \t")
	h.click("Log in")
	h.want("Enter the API key.")
	if len(submitted[state.Call](h.intents)) != 0 {
		t.Fatal("a blank key started a login")
	}
}

func TestRemoteEndpointOffersOnlyCallbackFreeFlows(t *testing.T) {
	remote := loginOn(t, endpointOf(box, "box", client.PhaseConnected))
	remote.choose("ChatGPT Signed in")
	remote.reject(flowLabels[flowBrowser])
	remote.want(flowLabels[flowManual], flowLabels[flowDevice])
	home := loginOn(t, endpointOf(local, "local", client.PhaseConnected))
	home.choose("ChatGPT Signed in")
	home.want(flowLabels[flowBrowser])
	home.click("Continue")
	if p := rawParams(t, home.call("dalgona/auth/start")); p["callbackMode"] != "backendLoopback" {
		t.Fatalf("browser flow callback %v", p["callbackMode"])
	}
}

func TestDeviceFlowShowsCodeAndDropsStatusFromAnotherGeneration(t *testing.T) {
	h := loginOn(t, endpointOf(box, "box", client.PhaseConnected))
	h.choose("ChatGPT Signed in")
	h.choose(flowLabels[flowDevice])
	h.click("Continue")
	start := h.call("dalgona/auth/start")
	if p := rawParams(t, start); p["flow"] != flowDevice || p["callbackMode"] != "device" {
		t.Fatalf("device start %v", p)
	}
	h.answer(start, authStatus{TransactionID: "tx-2", State: "waiting", Challenge: &authChallenge{Kind: flowDevice, VerificationURL: "https://example.com/device", UserCode: "WXYZ-1234", ExpiresAt: "2027-01-15T08:00:00.000Z"}})
	h.want("https://example.com/device", "Enter this code: WXYZ-1234", "Copy link", "Copy code", "Expires at", "Waiting for the sign-in to finish")
	h.wait(pollEvery)
	poll := h.call("dalgona/auth/status")
	h.calls[poll.ID] = state.CallView{ID: poll.ID, Endpoint: box, Generation: 2, State: state.CallSucceeded, Result: []byte(`{"transactionId":"tx-2","state":"succeeded"}`)}
	h.settle()
	h.want("The connection to the endpoint changed")
	h.reject("Logged in to ChatGPT.")
}

func TestClosingTheLoginPanelCancelsItsFlow(t *testing.T) {
	h := loginOn(t, endpointOf(box, "box", client.PhaseConnected))
	h.choose("ChatGPT Signed in")
	h.choose(flowLabels[flowManual])
	h.click("Continue")
	h.answer(h.call("dalgona/auth/start"), authStatus{TransactionID: "tx-3", State: "waiting", Challenge: &authChallenge{Kind: flowManual, AuthorizationURL: "https://auth.example.com/a", Prompt: "Paste the redirect URL or the authorization code"}})
	h.want("Paste the redirect URL or the authorization code", "Closing this panel cancels the sign-in.")
	h.take()
	h.click("Close")
	cancel := h.call("dalgona/auth/cancel")
	if p := params[authTransactionParams](t, cancel); p.TransactionID != "tx-3" || cancel.Endpoint != box {
		t.Fatalf("close cancelled %v on %s", p, cancel.Endpoint)
	}
}

func TestLateStartResponseIsCancelledAfterClose(t *testing.T) {
	h := loginOn(t, endpointOf(local, "local", client.PhaseConnected))
	h.choose("ChatGPT Signed in")
	h.click("Continue")
	start := h.call("dalgona/auth/start")
	h.running(start)
	h.click("Close")
	h.take()
	h.answer(start, authStatus{TransactionID: "tx-late", State: "waiting"})
	cancel := h.call("dalgona/auth/cancel")
	if p := params[authTransactionParams](t, cancel); p.TransactionID != "tx-late" {
		t.Fatalf("late start cancelled %v", p)
	}
}

func TestOAuthFailureOffersAFreshTryAgain(t *testing.T) {
	h := loginOn(t, endpointOf(local, "local", client.PhaseConnected))
	h.choose("ChatGPT Signed in")
	h.click("Continue")
	h.answer(h.call("dalgona/auth/start"), authStatus{TransactionID: "tx-4", State: authFailed, Error: "denied by the provider"})
	h.want("Sign-in failed: denied by the provider")
	h.click("Try again")
	if p := rawParams(t, h.call("dalgona/auth/start")); p["flow"] != flowBrowser {
		t.Fatalf("Try again started %v", p)
	}
}

func TestProviderDetailsShowAccountAndLogout(t *testing.T) {
	h := loginOn(t, endpointOf(local, "local", client.PhaseConnected))
	h.choose("ChatGPT Signed in")
	h.want("Signed in as me@example.com.")
	h.click("Log out")
	call := h.call("dalgona/auth/logout")
	if p := params[authTargetParams](t, call); p.Target != chatTarget || !call.Operation {
		t.Fatalf("logout %v", p)
	}
	h.answer(call, map[string]any{"signedIn": false})
	h.want("Logged out of ChatGPT. Models from it are no longer available; your drafts are kept.")
	if refresh := submitted[state.RefreshEndpoint](h.intents); len(refresh) != 1 {
		t.Fatal("logout did not refresh models")
	}
}
