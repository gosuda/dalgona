package rpcserver

import (
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"slices"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
)

const mcpConfigFeature = "mcpConfig"

func (s *server) mcpProvenance() bool {
	if s.native.Load() == nil {
		return false
	}
	index := slices.IndexFunc(nativeFeatures, func(f nativeFeature) bool { return f.name == mcpConfigFeature })
	return index >= 0 && nativeFeatures[index].offered(s)
}

func (s *server) nativeMCPStatus(ctx context.Context, p mcpStatusParams, cwd string) (jsontext.Value, error) {
	if p.configVersion == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	inventory, err := s.currentInventory(ctx, cwd, p.configVersion)
	if err != nil {
		return nil, err
	}
	start, err := boundCursorOffset(p.cursor, p.threadID, inventory.Version)
	if err != nil {
		return nil, err
	}
	statuses, err := attachedStatuses(s.host.MCPStatus(ctx, cwd), inventory, p.serverName)
	if err != nil {
		return nil, err
	}
	if _, err := s.currentInventory(ctx, cwd, inventory.Version); err != nil {
		return nil, err
	}
	start = min(start, len(statuses))
	end := cappedEnd(start, cmp.Or(p.limit, mcpStatusDefaultLimit), len(statuses))
	response := mcpStatusResponse{servers: statuses[start:end], configVersion: inventory.Version}
	if end < len(statuses) {
		response.nextCursor = statusCursor(end, p.threadID, inventory.Version)
	}
	return rpc.Marshal(response)
}

func (s *server) currentInventory(ctx context.Context, cwd, version string) (agent.MCPInventory, error) {
	inventory, err := s.host.MCPInventory(ctx, cwd)
	if err != nil {
		s.log.Warn("rpc: read MCP configuration", "error", err)
		return agent.MCPInventory{}, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if inventory.Version != version {
		return agent.MCPInventory{}, staleInventory(inventory.Version)
	}
	return inventory, nil
}

func attachedStatuses(statuses []agent.MCPServerStatus, inventory agent.MCPInventory, name string) ([]agent.MCPServerStatus, error) {
	effective := make(map[string]agent.MCPInventoryServer, len(inventory.Servers))
	for _, server := range inventory.Servers {
		if server.Effective {
			effective[server.Name] = server
		}
	}
	attached := make([]agent.MCPServerStatus, 0, len(statuses))
	for _, status := range statuses {
		switch {
		case status.Source == "":
			return nil, rpc.NativeKind(rpc.ErrorIncompatible)
		case !attaches(effective[status.Name], status):
			return nil, staleInventory(inventory.Version)
		case name == "" || status.Name == name:
			attached = append(attached, status)
		}
	}
	return attached, nil
}

func attaches(row agent.MCPInventoryServer, status agent.MCPServerStatus) bool {
	return row.Effective && row.Source == status.Source && row.ConfigDigest == status.ConfigDigest
}

func statusCursor(offset int, threadID, version string) string {
	return strconv.Itoa(offset) + "." + cursorBinding(threadID, version)
}

func boundCursorOffset(cursor, threadID, version string) (int, error) {
	if cursor == "" {
		return 0, nil
	}
	offset, binding, found := strings.Cut(cursor, ".")
	if !found || binding != cursorBinding(threadID, version) {
		return 0, staleInventory(version)
	}
	return cursorOffset(offset)
}

func cursorBinding(threadID, version string) string {
	sum := sha256.Sum256([]byte(threadID + "\x00" + version))
	return hex.EncodeToString(sum[:8])
}

func staleInventory(current string) error {
	return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: current})
}
