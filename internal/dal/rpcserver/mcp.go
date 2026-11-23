package rpcserver

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"slices"
	"strconv"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
)

const (
	mcpStatusDefaultLimit = 100
	mcpStatusMaxLimit     = 1000
)

type mcpStatusParams struct {
	cursor, serverName, threadID string
	limit                        int
}

func decodeMCPStatusParams(params jsontext.Value) (mcpStatusParams, error) {
	var p mcpStatusParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "cursor":
			p.cursor, err = rpc.ReadString(d)
		case "serverName":
			p.serverName, err = rpc.ReadString(d)
		case "threadId":
			p.threadID, err = rpc.ReadString(d)
		case "limit":
			p.limit, err = rpc.ReadInt(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	return p, err
}

func (s *server) mcpServerStatusList(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeMCPStatusParams(params)
	if err != nil {
		return nil, err
	}
	if p.limit > mcpStatusMaxLimit {
		return nil, invalidParams("limit must be at most " + strconv.Itoa(mcpStatusMaxLimit))
	}
	if p.limit < 0 {
		return nil, invalidParams("invalid limit " + strconv.Itoa(p.limit))
	}
	cwd, err := s.mcpWorkspace(p.threadID)
	if err != nil {
		return nil, err
	}
	start, err := cursorOffset(p.cursor)
	if err != nil {
		return nil, err
	}
	statuses := slices.DeleteFunc(s.host.MCPStatus(ctx, cwd), func(status agent.MCPServerStatus) bool {
		return p.serverName != "" && status.Name != p.serverName
	})
	start = min(start, len(statuses))
	limit := cmp.Or(p.limit, mcpStatusDefaultLimit)
	end := cappedEnd(start, limit, len(statuses))
	response := mcpStatusResponse{servers: statuses[start:end]}
	if end < len(statuses) {
		response.nextCursor = strconv.Itoa(end)
	}
	return rpc.Marshal(response)
}

func (s *server) mcpWorkspace(threadID string) (string, error) {
	if threadID == "" {
		return s.host.Config().Cwd, nil
	}
	info, ok := s.info(threadID)
	if !ok {
		return "", unknownThread(threadID)
	}
	return info.Cwd, nil
}

type mcpStatusResponse struct {
	servers    []agent.MCPServerStatus
	nextCursor string
}

func (r mcpStatusResponse) Encode(o *rpc.Writer) {
	o.Open()
	o.Key("data")
	o.OpenArray()
	for _, status := range r.servers {
		writeMCPServerStatus(o, status)
	}
	o.CloseArray()
	if r.nextCursor != "" {
		o.Str("nextCursor", r.nextCursor)
	}
	o.Close()
}

func writeMCPServerStatus(o *rpc.Writer, status agent.MCPServerStatus) {
	o.Open()
	o.Str("name", status.Name)
	o.Str("authStatus", cmp.Or(status.AuthStatus, "unsupported"))
	o.Key("tools")
	o.Open()
	for _, entry := range status.Tools {
		writeMCPTool(o, entry)
	}
	o.Close()
	o.Empty("resources")
	o.Empty("resourceTemplates")
	o.Str("runtimeStatus", status.Status)
	if status.Error != "" {
		o.Str("toolsError", status.Error)
	}
	o.Close()
}

func writeMCPTool(o *rpc.Writer, entry agent.MCPToolStatus) {
	o.Key(entry.Name)
	o.Open()
	o.Str("name", entry.Name)
	o.Str("description", entry.Spec.Description)
	schema := entry.Spec.Schema
	if len(schema) == 0 {
		schema = jsontext.Value(`{"type":"object"}`)
	}
	o.Raw("inputSchema", schema)
	o.Close()
}
