package main

import (
	"encoding/json/jsontext"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
	"github.com/gosuda/dalgona/internal/dalgona/remote"
)

func TestRemoteResultSchemaFollowsNativeNegotiation(t *testing.T) {
	t.Run("legacy", requireLegacyRemoteSchema)
	t.Run("native", requireNativeRemoteSchema)
}

func requireLegacyRemoteSchema(t *testing.T) {
	t.Helper()
	peer := remoteSchemaPeer(t, false)
	oversize := strings.Repeat("x", (64<<10)+1)
	for method, params := range map[string]remoteCall{methodRemotePull: {ThreadID: oversize}, methodRemoteGC: {}} {
		var got map[string]jsontext.Value
		if err := peer.Call(t.Context(), method, params, &got); err != nil {
			t.Fatal(err)
		}
		if len(got) != 2 || got["exitCode"] == nil || got["output"] == nil {
			t.Errorf("%s did not return exactly exitCode and output", method)
		}
		if method == methodRemotePull && !strings.Contains(string(got["output"]), oversize) {
			t.Error("legacy pull truncated its output")
		}
	}
}

func requireNativeRemoteSchema(t *testing.T) {
	t.Helper()
	peer := remoteSchemaPeer(t, true)
	for method, field := range map[string]string{methodRemotePull: "changedPaths", methodRemoteGC: "previewRevision"} {
		var got map[string]jsontext.Value
		if err := peer.Call(t.Context(), method, remoteCall{}, &got); err != nil || got[field] == nil {
			t.Errorf("%s returned %v, %v; want native field %q", method, got, err, field)
		}
	}
}

func remoteSchemaPeer(t *testing.T, native bool) *rpc.Conn {
	t.Helper()
	store, err := journal.Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	rig := newNativeRig(t, func(string) {}, func(home string) rpcserver.ServeOptions {
		d := &battery.Daemon{Remote: remote.Options{Config: config.Config{Home: home}, Journal: store}}
		return rpcserver.ServeOptions{Extensions: remoteExtensions(d)}
	})
	params := rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: "schema-test"}}
	if native {
		params.Capabilities = &rpc.InitializeCapabilities{DalgonaNative: &rpc.NativeCapability{Version: rpc.NativeVersion, Delivery: rpc.NativeDelivery}}
	}
	var result rpc.InitializeResult
	if err := rig.peer.Initialize(t.Context(), params, &result); err != nil {
		t.Fatal(err)
	}
	return rig.peer
}
