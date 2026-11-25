package rpcserver

import (
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestNativeCapabilitiesAdvertiseOnlyRegisteredFeatures(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	first := r.connectNative(ServeOptions{})
	second := r.connectNative(ServeOptions{Extensions: map[string]Extension{"dalgona/diagnostics/run": nil}})
	if first.streamID == "" || first.streamID == second.streamID {
		t.Fatalf("stream ids %q and %q are not fresh per connection", first.streamID, second.streamID)
	}
	var caps rpc.Capabilities
	second.ok("dalgona/capabilities/read", map[string]any{}, &caps)
	checkCoreFeatures(t, caps, second)
	checkCapabilityLimits(t, caps)
}

func checkCoreFeatures(t *testing.T, caps rpc.Capabilities, second *nativeClient) {
	t.Helper()
	for _, name := range []string{"orderedEvents", "requestCancellation", "history", "mutationReceipts", "threadUnsubscribe", "diagnostics"} {
		if caps.Features[name] != 1 {
			t.Fatalf("feature %s = %d, want 1 in %v", name, caps.Features[name], caps.Features)
		}
	}
	for _, name := range []string{"terminal", "workspace", "auth"} {
		if _, ok := caps.Features[name]; ok && !second.registeredAll(name) {
			t.Fatalf("feature %s advertised without its methods", name)
		}
	}
	if !slices.Contains(caps.ReceiptMethods, "turn/start") || slices.Contains(caps.ReceiptMethods, "thread/read") {
		t.Fatalf("receiptMethods = %v", caps.ReceiptMethods)
	}
}

func checkCapabilityLimits(t *testing.T, caps rpc.Capabilities) {
	t.Helper()
	want := rpc.CapabilityLimits{FrameBytes: 16777216, ResultBytes: 2097152, Subscriptions: 8, SnapshotHandles: 8, WorkspaceHandles: 8, TerminalSessions: 4}
	if caps.Limits != want || caps.DaemonInstanceID == "" || caps.Version != 1 {
		t.Fatalf("capabilities = %+v", caps)
	}
}

func (c *nativeClient) registeredAll(feature string) bool {
	for _, f := range nativeFeatures {
		if f.name == feature {
			return !slices.ContainsFunc(f.methods, func(name string) bool { _, ok := methods[name]; return !ok })
		}
	}
	return false
}

func TestInitializeRefusesIncompatibleNativeContract(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	for _, params := range []map[string]any{nativeInitialize(2, rpc.NativeDelivery), nativeInitialize(1, "ordered-v2")} {
		failure := c.failsWith("initialize", params, rpc.ErrorIncompatible)
		if failure.Code != -32011 {
			t.Fatalf("incompatible code = %d, want -32011", failure.Code)
		}
	}
	legacy := r.connect()
	var init struct {
		DalgonaNative *rpc.NativeSession `json:"dalgonaNative"`
	}
	legacy.ok("initialize", map[string]any{"clientInfo": map[string]string{"name": "legacy", "version": "1"}}, &init)
	if init.DalgonaNative != nil {
		t.Fatal("a client without the capability received dalgonaNative")
	}
}
