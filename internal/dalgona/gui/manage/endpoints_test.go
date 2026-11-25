//go:build gui

package manage

import (
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const boxEndpoint client.EndpointID = "boxy"

func endpointsRig(t *testing.T, service string) *rig {
	r := newRig(t)
	r.daemon.mu.Lock()
	r.daemon.statuses = append(r.daemon.statuses, client.EndpointStatus{ID: boxEndpoint, Label: "boxy", Kind: client.EndpointBox})
	r.daemon.mu.Unlock()
	r.life.specs[string(boxEndpoint)] = client.RemoteSpec{Kind: client.RemoteBox, Service: service, BoxID: "box-1"}
	r.connect(localEndpoint, remoteEndpoint)
	r.panels.Endpoints.Opened(shell.Route{Panel: "remote"})
	r.frame(r.panels.Endpoints)
	return r
}

func TestEndpointsShowEachPhaseAndActOnlyOnTheirTarget(t *testing.T) {
	r := endpointsRig(t, "e2b")
	p := r.panels.Endpoints
	r.mustShow("Connected · Local daemon · " + string(localEndpoint))
	r.mustShow("Connected · Remote daemon · builder")
	r.mustShow("Idle · Daemon in a box · boxy")
	r.click(p, "Connect boxy")
	if want := state.Intent(state.Connect{Endpoint: boxEndpoint}); !slices.Equal(r.submitted, []state.Intent{want}) {
		t.Fatalf("Connect submitted %v, want only %v", r.submitted, want)
	}
	r.click(p, "Disconnect builder")
	r.settle(p, &p.work)
	if !slices.Equal(r.life.disc, []client.EndpointID{remoteEndpoint}) {
		t.Fatalf("Disconnect reached %v, want only builder", r.life.disc)
	}
	r.await(func(v *state.View) bool { return bind(v, remoteEndpoint).phase == client.PhaseOffline })
	r.frame(p)
	r.mustShow("Offline · Remote daemon · builder")
	r.mustShow("Connected · Local daemon · " + string(localEndpoint))
}

func TestEndpointsAddRemoteKeepsFieldsUntilTheLifecycleAccepts(t *testing.T) {
	r := endpointsRig(t, "e2b")
	p := r.panels.Endpoints
	refusal := errors.New("remotes.edge.url must use https or wss")
	r.life.saveFn = func(name string, spec client.RemoteSpec) error {
		if spec.URL != "wss://edge.example" {
			return refusal
		}
		return nil
	}
	r.click(p, "Add remote")
	r.typeInto(p, "Remote name", "edge")
	r.click(p, "Remote daemon (url and token file) instead of a daemon in a box")
	r.typeInto(p, "Daemon URL (https or wss)", "ftp://edge.example")
	r.typeInto(p, "Token file path", "/home/test/edge.token")
	r.click(p, "Save remote")
	r.settle(p, &p.work)
	r.mustShow("Not saved: " + refusal.Error())
	if p.form.url.text() != "ftp://edge.example" || !p.form.open {
		t.Fatal("a refused save must keep the form and its fields")
	}
	r.typeInto(p, "Daemon URL (https or wss)", "wss://edge.example")
	r.click(p, "Save remote")
	r.settle(p, &p.work)
	spec := r.life.specs["edge"]
	if spec.Kind != client.RemoteDaemon || spec.URL != "wss://edge.example" || spec.TokenFile != "/home/test/edge.token" || spec.Service != "" {
		t.Fatalf("saved spec %+v", spec)
	}
	r.mustShow("Idle · Remote daemon · edge")
	r.mustShow("Saved remote edge.")
}

func TestCloudUpLeavesCredentialsUncopiedAndOffersLogin(t *testing.T) {
	r := endpointsRig(t, "e2b")
	p := r.panels.Endpoints
	r.chosen = "/work/seed"
	r.click(p, "Cloud up boxy")
	if !r.button("Start box").Desc.Disabled {
		t.Fatal("Cloud up must wait for a chosen workspace")
	}
	r.click(p, "Choose folder")
	r.settle(p, &p.work)
	r.mustShow("Folder: /work/seed")
	r.click(p, "Start box")
	r.settle(p, &p.work)
	if len(r.life.ups) != 1 {
		t.Fatalf("CloudUp ran %d times", len(r.life.ups))
	}
	up := r.life.ups[0]
	if up.Endpoint != boxEndpoint || up.Workspace != "/work/seed" || up.CopyAuth || up.OperationID == "" {
		t.Fatalf("CloudUp request %+v; want boxy, the chosen folder, no copied credentials, an operation id", up)
	}
	r.mustShow("Box box-9 on e2b is ready for boxy.")
	r.click(p, "Sign in on boxy")
	want := shell.Route{Panel: "login", Thread: state.ThreadKey{Endpoint: boxEndpoint}}
	if !slices.Contains(r.opened, want) {
		t.Fatalf("login route %v, want %v", r.opened, want)
	}
}

func TestCloudUpShowsProgressAndCancelsOnlyOnRequest(t *testing.T) {
	r := endpointsRig(t, "e2b")
	p := r.panels.Endpoints
	r.life.upGate = make(chan struct{})
	r.chosen = "/work/seed"
	r.click(p, "Cloud up boxy")
	r.click(p, "Choose folder")
	r.settle(p, &p.work)
	r.click(p, "Start box")
	for range 1000 {
		r.frame(p)
		if r.shows("Cloud up: connecting (0 of 2)") {
			break
		}
		sleepBriefly()
	}
	r.mustShow("Cloud up: connecting (0 of 2)")
	r.click(p, "Cancel Cloud up")
	r.settle(p, &p.work)
	r.mustShow("Cloud up stopped on request.")
}

func TestBoxRemovalNamesServiceAndBoxThenSendsTheConfirmedBox(t *testing.T) {
	r := endpointsRig(t, "e2b")
	p := r.panels.Endpoints
	r.click(p, "Remove box of boxy")
	r.mustShow("Remove box box-1 on e2b for boxy?")
	r.mustShow("destroys the remote daemon's journal and every thread stored in that box")
	r.click(p, "Remove box box-1")
	r.settle(p, &p.work)
	if len(r.life.downs) != 1 || r.life.downs[0].Box != "box-1" || r.life.downs[0].Endpoint != boxEndpoint {
		t.Fatalf("CloudDown requests %+v; want box-1 on boxy", r.life.downs)
	}
	r.mustShow("The box was removed")
}

func TestCloudflareBoxesRefuseCloudOperations(t *testing.T) {
	r := endpointsRig(t, "cloudflare")
	p := r.panels.Endpoints
	r.click(p, "Cloud up boxy")
	r.mustShow(cloudflareRefusal)
	if _, ok := r.find("Start box"); ok {
		t.Fatal("a cloudflare box must not offer Cloud up")
	}
}
