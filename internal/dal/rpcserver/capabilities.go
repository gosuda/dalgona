package rpcserver

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"runtime"
	"slices"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

const (
	terminalCheckpointVersion = 1
	frameBytes                = rpc.FrameLimit
	resultBytes               = 2 << 20
	connectionSubscriptions   = 8
	connectionSnapshots       = 8
	daemonSnapshots           = 64
	connectionWorkspaces      = 8
	connectionTerminals       = 4
	handleBytes               = 16
)

type nativeFeature struct {
	name      string
	version   int
	methods   []string
	available func(*server) bool
}

func (f nativeFeature) offered(s *server) bool {
	if f.available != nil && !f.available(s) {
		return false
	}
	return !slices.ContainsFunc(f.methods, func(name string) bool { return !s.registered(name) })
}

var nativeFeatures = []nativeFeature{
	{name: "orderedEvents", version: 1},
	{name: "requestCancellation", version: 1},
	{name: "history", version: 1, methods: []string{"dalgona/history/open", "dalgona/history/page", "dalgona/history/content/read", "dalgona/history/close"}},
	{name: "mutationReceipts", version: 1, methods: []string{"dalgona/operation/read"}},
	{name: "imageRead", version: 1, methods: []string{"dalgona/item/image/read"}},
	{name: "threadUnsubscribe", version: 1, methods: []string{"thread/unsubscribe", "dalgona/request/details/read"}},
	{name: "auth", version: 1, methods: []string{"dalgona/auth/providers", "dalgona/auth/start", "dalgona/auth/submit", "dalgona/auth/status", "dalgona/auth/cancel", "dalgona/auth/logout"}},
	{name: "mcpConfig", version: 1, methods: []string{"dalgona/mcp/config/read", "dalgona/mcp/config/write"}},
	{name: "workspace", version: 1, methods: []string{
		"dalgona/workspace/open", "dalgona/workspace/close", "dalgona/workspace/list", "dalgona/workspace/exec",
		"dalgona/workspace/search", "dalgona/workspace/stat", "dalgona/workspace/read", "dalgona/workspace/write",
		"dalgona/workspace/mkdir", "dalgona/workspace/rename", "dalgona/workspace/remove", "dalgona/workspace/watch", "dalgona/workspace/unwatch",
	}},
	{name: "terminal", version: 1, methods: []string{
		"dalgona/terminal/open", "dalgona/terminal/list", "dalgona/terminal/attach", "dalgona/terminal/checkpoint/open",
		"dalgona/terminal/checkpoint/read", "dalgona/terminal/read", "dalgona/terminal/write", "dalgona/terminal/resize",
		"dalgona/terminal/close", "dalgona/terminal/detach",
	}, available: func(s *server) bool { return s.opts.Terminals != nil }},
	{name: "runnerManagement", version: 1, methods: []string{"dalgona/remote/status", "dalgona/remote/pull", "dalgona/remote/gc"}},
	{name: "diagnostics", version: 1, methods: []string{"dalgona/diagnostics/run"}},
	{name: "failureRecords", version: 1, methods: []string{"dalgona/failures/list", "dalgona/failures/read"}},
	{name: "reporting", version: 1, methods: []string{"dalgona/reporting/read", "dalgona/reporting/enable", "dalgona/reporting/disable"}},
}

var daemonInstance = sync.OnceValue(newHandle)

type nativeSession struct {
	streamID  string
	contents  *liveContents
	histories *historyTable
}

func newHandle() string {
	raw := make([]byte, handleBytes)
	if _, err := rand.Read(raw); err != nil {
		panic(fmt.Sprintf("rpcserver: read random handle: %v", err))
	}
	return base64.RawURLEncoding.EncodeToString(raw)
}

func capabilityMethods() map[string]method {
	return map[string]method{
		"initialize":                (*server).negotiate,
		"dalgona/capabilities/read": deadline(MetadataDeadline, (*server).capabilitiesRead),
	}
}

func (s *server) negotiate(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	requested, err := nativeRequest(params)
	if err != nil {
		return nil, err
	}
	if requested != nil && (requested.Version != rpc.NativeVersion || requested.Delivery != rpc.NativeDelivery) {
		return nil, rpc.NativeKind(rpc.ErrorIncompatible)
	}
	base, err := s.initialize(ctx, params)
	if err != nil || requested == nil {
		return base, err
	}
	session := s.openNative()
	member, err := json.Marshal(rpc.NativeSession{Version: rpc.NativeVersion, Delivery: rpc.NativeDelivery, DaemonInstanceID: daemonInstance(), StreamID: session.streamID})
	if err != nil {
		return nil, internalError(err)
	}
	trimmed := bytes.TrimSuffix(bytes.TrimSpace(base), []byte("}"))
	return slices.Concat(trimmed, []byte(`,"dalgonaNative":`), member, []byte("}")), nil
}

func nativeRequest(params jsontext.Value) (*rpc.NativeCapability, error) {
	if len(params) == 0 {
		return nil, nil
	}
	var p struct {
		Capabilities *struct {
			DalgonaNative *rpc.NativeCapability `json:"dalgonaNative"`
		} `json:"capabilities"`
	}
	if err := json.Unmarshal(params, &p); err != nil {
		return nil, invalidParams("invalid dalgonaNative capability: " + err.Error())
	}
	if p.Capabilities == nil {
		return nil, nil
	}
	return p.Capabilities.DalgonaNative, nil
}

func (s *server) openNative() *nativeSession {
	s.mu.Lock()
	defer s.mu.Unlock()
	if session := s.native.Load(); session != nil {
		return session
	}
	session := &nativeSession{streamID: newHandle(), contents: newLiveContents(), histories: newHistoryTable()}
	s.conn.OpenStream(session.streamID, session.spillEvent)
	s.native.Store(session)
	return session
}

func (s *server) capabilitiesRead(context.Context, jsontext.Value) (jsontext.Value, error) {
	features := map[string]int{}
	for _, feature := range nativeFeatures {
		if feature.offered(s) {
			features[feature.name] = feature.version
		}
	}
	caps := rpc.Capabilities{
		Version: rpc.NativeVersion, DaemonInstanceID: daemonInstance(), BackendBuild: config.Build(),
		Platform: rpc.Platform{OS: runtime.GOOS, Arch: runtime.GOARCH, PathStyle: pathStyle()},
		Features: features, ReceiptMethods: s.receiptMethods(),
		Limits: rpc.CapabilityLimits{
			FrameBytes: frameBytes, ResultBytes: resultBytes, Subscriptions: connectionSubscriptions,
			SnapshotHandles: connectionSnapshots, WorkspaceHandles: connectionWorkspaces, TerminalSessions: connectionTerminals,
		},
	}
	if _, ok := features["terminal"]; ok {
		caps.TerminalCheckpointVersion = terminalCheckpointVersion
	}
	raw, err := json.Marshal(caps)
	if err != nil {
		return nil, internalError(err)
	}
	return raw, nil
}

func (s *server) registered(name string) bool {
	_, core := methods[name]
	_, extension := s.opts.Extensions[name]
	_, operation := s.opts.OperationExtensions[name]
	return core || extension || operation
}

func pathStyle() string {
	if runtime.GOOS == "windows" {
		return "windows"
	}
	return "posix"
}
