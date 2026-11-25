//go:build gui

package main

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/manage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type notification struct {
	link   client.Link
	method string
	params string
}

type recordingSink struct{ got []notification }

func (r *recordingSink) Notified(link client.Link, method string, params jsontext.Value) {
	r.got = append(r.got, notification{link: link, method: method, params: string(params)})
}

func TestComposedObserverForwardsWorkspaceChangesToTheHub(t *testing.T) {
	o := &observer{Owner: state.New(func() {}, t.TempDir())}
	link := client.Link{Endpoint: "/home/u/.dalgona", Generation: 1}
	params := jsontext.Value(`{"watchId":"watch-1","directoryRevision":"d2","paths":["new.txt"],"rescanRequired":false}`)
	o.Notified(link, "dalgona/workspace/changed", params)
	sink := &recordingSink{}
	o.forward(sink)
	o.Notified(link, "dalgona/workspace/changed", params)
	want := notification{link: link, method: "dalgona/workspace/changed", params: string(params)}
	if len(sink.got) != 1 || sink.got[0] != want {
		t.Fatalf("the hub received %+v, want only %+v", sink.got, want)
	}
}

func TestApplicationForwardsNotificationsToItsManageHub(t *testing.T) {
	a := newApplication(t.Context(), t.TempDir(), nil)
	hub := manage.NewHub(t.Context(), manage.Deps{})
	a.attachHub(hub)
	if got := a.observer.sink.Load(); got == nil || *got != notifiedSink(hub) {
		t.Fatal("the endpoint observer does not forward notifications to the manage hub")
	}
}
