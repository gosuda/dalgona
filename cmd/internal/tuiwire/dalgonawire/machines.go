package dalgonawire

import (
	"context"
	"encoding/json/jsontext"

	"github.com/gosuda/dalgona/cmd/internal/tuiwire"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/tui"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func Launch(cfg config.Config, initial string, openRemotes func() (*client.Remotes, error)) app.TUIBackend {
	return tuiwire.Launch(initial, func(events tui.MachineEvents) (tui.Machines, error) {
		backend, err := client.CurrentBackend()
		if err != nil {
			return nil, err
		}
		remotes, err := openRemotes()
		if err != nil {
			return nil, err
		}
		set, err := client.NewEndpointSet(remotes, client.ConfiguredEndpoints(cfg, backend, remotes), observer{events: events}, client.ProtocolTUI)
		if err != nil {
			return nil, err
		}
		return machines{set: set}, nil
	})
}

type machines struct{ set *client.EndpointSet }

func (m machines) Endpoints() []tui.Machine {
	endpoints := m.set.Endpoints()
	rows := make([]tui.Machine, 0, len(endpoints))
	for _, endpoint := range endpoints {
		rows = append(rows, tui.Machine{ID: string(endpoint.ID), Label: endpoint.Label, Kind: kind(endpoint.Kind), Phase: endpoint.Phase.String()})
	}
	return rows
}

func kind(k client.EndpointKind) string {
	switch k {
	case client.EndpointLocal:
		return "local"
	case client.EndpointBox:
		return "box"
	case client.EndpointDaemon:
		return "daemon relay"
	case client.EndpointSSH:
		return "SSH daemon"
	case client.EndpointDevice:
		return "paired device"
	}
	return "unknown transport"
}

func (m machines) Connect(id string) error { return m.set.Connect(client.EndpointID(id)) }

func (m machines) Disconnect(id string) error { return m.set.Disconnect(client.EndpointID(id)) }

func (m machines) Attach(id string) (func(), error) { return m.set.Attach(client.EndpointID(id)) }

func (m machines) Call(ctx context.Context, link tui.Link, method string, params, result any) error {
	return m.set.Call(ctx, clientLink(link), method, params, result)
}

func (m machines) Respond(ctx context.Context, link tui.Link, requestID string, result any) error {
	return m.set.Respond(ctx, clientLink(link), requestID, result)
}

func (m machines) Close() error { return m.set.Close() }

func (m machines) OnboardSSH(ctx context.Context, name, target, artifact string) error {
	return m.set.OnboardSSH(ctx, name, client.SSHTarget{Destination: target, Artifact: artifact})
}

func (m machines) PairDevice(ctx context.Context, name string, card rpc.PairingCard, code, label string) error {
	return m.set.PairDevice(ctx, name, card, code, label)
}

func clientLink(link tui.Link) client.Link {
	return client.Link{Endpoint: client.EndpointID(link.Endpoint), Generation: client.Generation(link.Generation)}
}

func terminalLink(link client.Link) tui.Link {
	return tui.Link{Endpoint: string(link.Endpoint), Generation: uint64(link.Generation)}
}

type observer struct{ events tui.MachineEvents }

func (o observer) PhaseChanged(change client.PhaseChange) {
	o.events.Phase(tui.MachinePhase{Link: terminalLink(change.Link), Phase: change.Phase.String(), Initialized: change.Initialized, Err: change.Err})
}

func (o observer) Notified(link client.Link, method string, params jsontext.Value) {
	o.events.Notify(terminalLink(link), method, params)
}

func (o observer) Requested(ctx context.Context, link client.Link, method string, params jsontext.Value) (any, error) {
	return o.events.Request(ctx, terminalLink(link), method, params)
}
