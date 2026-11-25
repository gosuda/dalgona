//go:build gui

package manage

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestDiagnosticsWaitForAnExplicitRunAndKeepTheirTarget(t *testing.T) {
	r := newRig(t)
	r.connect(remoteEndpoint)
	p := r.panels.Diagnostics
	r.life.diag = []DiagnosticRow{{Status: "ok", Check: "config", Detail: "settings.toml parsed"}}
	r.daemon.onAt(remoteEndpoint, methodDiagnosticsRun, func(c call) (any, error) {
		return diagnosticsResult{Rows: []diagnosticWire{{Status: "fail", Check: "provider", Detail: "remote provider refused"}}, ExitCode: 1}, nil
	})
	p.Opened(shell.Route{Panel: "doctor", Thread: state.ThreadKey{Endpoint: localEndpoint}})
	r.settle(p, &p.work)
	if len(r.life.diags) != 0 || len(r.daemon.sent(methodDiagnosticsRun)) != 0 {
		t.Fatal("opening diagnostics must not run checks")
	}
	r.click(p, "Run local checks")
	r.settle(p, &p.work)
	if len(r.life.diags) != 1 || r.life.diags[0] {
		t.Fatalf("a disconnected local endpoint must run the lifecycle Diagnose offline; got %v", r.life.diags)
	}
	r.mustShow("OK · config · settings.toml parsed")
	r.click(p, "Diagnose builder · Connected")
	r.click(p, "Run online checks")
	r.settle(p, &p.work)
	runs := r.daemon.sent(methodDiagnosticsRun)
	if len(runs) != 1 || runs[0].link.Endpoint != remoteEndpoint || !params[diagnosticsParams](t, runs[0]).Online {
		t.Fatalf("online checks %+v; want one online run on builder", runs)
	}
	if len(r.life.diags) != 1 {
		t.Fatal("remote checks must not fall back to local diagnostics")
	}
	r.mustShow("Failed · provider · remote provider refused")
	r.mustHide("settings.toml parsed")
}

func reportingRig(t *testing.T) *rig {
	r := newRig(t)
	r.connect(remoteEndpoint)
	r.daemon.on(methodFailuresList, func(call) (any, error) { return failuresPage{}, nil })
	r.daemon.on(methodAuthProviders, func(call) (any, error) { return authProviders{}, nil })
	r.daemon.on(methodReportingRead, func(c call) (any, error) {
		method := params[reportingReadParams](t, c).Method
		return reportingState{Method: cmpOr(method, "gh"), Available: true, ConsentText: "Send masked excerpts of " + cmpOr(method, "gh") + " failures.", TextHash: "hash-" + cmpOr(method, "gh"), Authorization: "ready"}, nil
	})
	p := r.panels.Failures
	p.Opened(shell.Route{Panel: "failures", Thread: remoteThread})
	r.settle(p, &p.work)
	return r
}

func TestReportingEnableRequiresTypedYesAndSendsTheShownHash(t *testing.T) {
	r := reportingRig(t)
	p := r.panels.Failures
	r.daemon.on(methodReportingEnable, func(call) (any, error) {
		return enabled{ConsentReceipt: consentReceipt{SchemaVersion: 1, Method: "gh", TextHash: "hash-gh", Time: "2026-11-24T00:00:00.000Z"}, Authorization: "ready"}, nil
	})
	r.mustShow("Send masked excerpts of gh failures.")
	r.typeInto(p, "Type yes to agree to the text above", "Yes")
	if !r.button("Enable reporting").Desc.Disabled {
		t.Fatal("only the exact word yes may enable reporting")
	}
	r.click(p, "Enable reporting")
	r.settle(p, &p.work)
	if n := len(r.daemon.sent(methodReportingEnable)); n != 0 {
		t.Fatalf("a mistyped confirmation sent %d enables", n)
	}
	r.typeInto(p, "Type yes to agree to the text above", "yes")
	r.click(p, "Enable reporting")
	r.settle(p, &p.work)
	sent := r.daemon.sent(methodReportingEnable)
	if len(sent) != 1 {
		t.Fatalf("enable sent %d times", len(sent))
	}
	got := params[enableParams](t, sent[0])
	if got.Method != "gh" || got.TextHash != "hash-gh" || got.Confirmation != "yes" || got.OperationID == "" || sent[0].link.Endpoint != remoteEndpoint {
		t.Fatalf("enable request %+v on %s", got, sent[0].link.Endpoint)
	}
	r.mustShow("Consent receipt stored")
}

func TestReportingMethodChangeAsksForFreshConsentAndDisableKeepsIssues(t *testing.T) {
	r := reportingRig(t)
	p := r.panels.Failures
	r.daemon.on(methodReportingDisable, func(call) (any, error) {
		return disabled{QueuedReportsDeleted: true}, nil
	})
	r.typeInto(p, "Type yes to agree to the text above", "yes")
	r.click(p, "Report with app")
	r.settle(p, &p.work)
	r.mustShow("Send masked excerpts of app failures.")
	if p.report.confirm.text() != "" {
		t.Fatal("changing the method must discard the earlier confirmation")
	}
	r.click(p, "Disable reporting")
	r.settle(p, &p.work)
	if len(r.daemon.sent(methodReportingDisable)) != 1 {
		t.Fatal("Disable must call dalgona/reporting/disable")
	}
	r.mustShow("Issues already published on GitHub stay published.")
}
