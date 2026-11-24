//go:build gui

package manage

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

func runnerRig(t *testing.T) *rig {
	r := newRig(t)
	r.daemon.thread(localThread, "Local name", "", "/work/local")
	r.daemon.thread(remoteThread, "Remote name", "", "/work/remote")
	r.connect(localEndpoint, remoteEndpoint)
	r.open(localThread)
	r.open(remoteThread)
	r.thread = remoteThread
	return r
}

func TestRunnerReadsItsThreadsBindingOnlyFromThatEndpoint(t *testing.T) {
	r := runnerRig(t)
	p := r.panels.Runner
	r.daemon.onAt(localEndpoint, methodRemoteStatus, func(call) (any, error) {
		return remoteStatus{Bindings: []remoteBinding{{ThreadID: "t1", RunnerID: "other", BoxID: "box-local", State: "bound"}}}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodRemoteStatus, func(call) (any, error) {
		return remoteStatus{
			Runners:  []remoteRunner{{Name: "e2b", Service: "e2b", Available: true, Capabilities: []string{"pull"}}},
			Bindings: []remoteBinding{{ThreadID: "t1", RunnerID: "e2b", BoxID: "box-1", State: "bound", SharedThreadIDs: []string{"t7"}, BindingRevision: "rev-9"}},
		}, nil
	})
	p.Opened(shell.Route{Panel: "runner", Thread: remoteThread})
	r.settle(p, &p.work)
	sent := r.daemon.sent(methodRemoteStatus)
	if len(sent) != 1 || sent[0].link.Endpoint != remoteEndpoint || params[remoteStatusParams](t, sent[0]).ThreadID != "t1" {
		t.Fatalf("remote/status calls %+v; want one on builder naming t1", sent)
	}
	r.mustShow("Remote daemon: the daemon on builder owns this thread.")
	r.mustShow("Remote archive execution: tools run in box box-1 on runner e2b.")
	r.mustShow("Workspace root: /work/remote on this endpoint, mapped to /workspace in box box-1")
	r.mustShow("Binding bound on runner e2b, shared with t7; binding revision rev-9.")
	r.mustShow("e2b · e2b · available · pull")
	r.mustHide("box-local")
}

func TestRunnerReportsLocalExecutionWhenNoBoxIsBound(t *testing.T) {
	r := runnerRig(t)
	p := r.panels.Runner
	r.daemon.on(methodRemoteStatus, func(call) (any, error) { return remoteStatus{}, nil })
	p.Opened(shell.Route{Panel: "runner", Thread: localThread})
	r.settle(p, &p.work)
	r.mustShow("Local execution: tools run on the host of " + string(localEndpoint) + ".")
	r.mustShow("Workspace root: /work/local")
	r.mustHide("Remote daemon")
	r.mustHide("Remote archive execution")
}

func TestRunnerGarbageCollectionRemovesThePreviewedBoxesAndReportsTheDaemonResult(t *testing.T) {
	r := runnerRig(t)
	p := r.panels.Runner
	r.daemon.on(methodRemoteStatus, func(call) (any, error) { return remoteStatus{}, nil })
	r.daemon.onAt(remoteEndpoint, methodRemoteGC, func(c call) (any, error) {
		if !params[gcParams](t, c).Yes {
			return gcResult{PreviewRevision: "ledger-4", Candidates: []gcCandidate{
				{BoxID: "box-a", Service: "e2b", ThreadIDs: []string{"t3"}, Reason: "archived"},
				{BoxID: "box-b", Service: "e2b", Reason: "orphaned"},
			}}, nil
		}
		return gcResult{Destroyed: []string{"box-a"}, Failures: []gcFailure{{BoxID: "box-b", Kind: "unavailable"}}}, nil
	})
	p.Opened(shell.Route{Panel: "runner", Thread: remoteThread})
	r.settle(p, &p.work)
	r.click(p, "Preview removable boxes")
	r.settle(p, &p.work)
	r.mustShow("Confirming removes these 2 boxes on builder and every file inside them:")
	r.mustShow("box-a on runner service e2b · threads t3 · archived")
	r.mustShow("box-b on runner service e2b · threads none · orphaned")
	r.click(p, "Remove 2 boxes on builder")
	r.settle(p, &p.work)
	gcs := r.daemon.sent(methodRemoteGC)
	if len(gcs) != 2 {
		t.Fatalf("remote/gc sent %d times, want a preview and one confirmation", len(gcs))
	}
	preview, confirm := params[gcParams](t, gcs[0]), params[gcParams](t, gcs[1])
	if preview.Yes || preview.PreviewRevision != "" || !confirm.Yes || confirm.PreviewRevision != "ledger-4" {
		t.Fatalf("gc params preview=%+v confirm=%+v; the confirmation must name the previewed ledger revision", preview, confirm)
	}
	if gcs[1].operation == "" || gcs[1].operation == gcs[0].operation {
		t.Fatal("the confirmed removal must carry its own operation identifier")
	}
	r.mustShow("The daemon removed 1 boxes: box-a.")
	r.mustShow("Box box-b was not removed (unavailable).")
	r.mustHide("Confirming removes")
}

func TestRunnerGarbageCollectionStopsWhenTheLedgerChangedSinceThePreview(t *testing.T) {
	r := runnerRig(t)
	p := r.panels.Runner
	r.daemon.on(methodRemoteStatus, func(call) (any, error) { return remoteStatus{}, nil })
	r.daemon.on(methodRemoteGC, func(c call) (any, error) {
		if params[gcParams](t, c).Yes {
			return nil, rpc.NativeKind(rpc.ErrorStaleRevision)
		}
		return gcResult{PreviewRevision: "ledger-4", Candidates: []gcCandidate{{BoxID: "box-a", Service: "e2b", Reason: "archived"}}}, nil
	})
	p.Opened(shell.Route{Panel: "runner", Thread: remoteThread})
	r.settle(p, &p.work)
	r.click(p, "Preview removable boxes")
	r.settle(p, &p.work)
	r.click(p, "Remove 1 boxes on builder")
	r.settle(p, &p.work)
	r.mustShow("The box ledger changed since the preview; nothing was removed. Preview again.")
	r.mustHide("The daemon removed")
	if _, ok := r.find("Remove 1 boxes on builder"); ok {
		t.Fatal("a stale preview must not stay confirmable")
	}
}
