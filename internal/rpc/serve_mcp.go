package rpc

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"slices"
	"strconv"

	"github.com/gosuda/dalgona/internal/agent"
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
			p.cursor, err = readString(d)
		case "serverName":
			p.serverName, err = readString(d)
		case "threadId":
			p.threadID, err = readString(d)
		case "limit":
			p.limit, err = readInt(d)
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
	return encode(response)
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

func (r mcpStatusResponse) write(o *out) {
	o.open()
	o.key("data")
	o.openArray()
	for _, status := range r.servers {
		writeMCPServerStatus(o, status)
	}
	o.closeArray()
	if r.nextCursor != "" {
		o.str("nextCursor", r.nextCursor)
	}
	o.close()
}

func writeMCPServerStatus(o *out, status agent.MCPServerStatus) {
	o.open()
	o.str("name", status.Name)
	o.str("authStatus", "unsupported")
	o.key("tools")
	o.open()
	for _, entry := range status.Tools {
		writeMCPTool(o, entry)
	}
	o.close()
	o.empty("resources")
	o.empty("resourceTemplates")
	o.str("runtimeStatus", status.Status)
	if status.Status == "failed" && status.Error != "" {
		o.str("toolsError", status.Error)
	}
	o.close()
}

func writeMCPTool(o *out, entry agent.MCPToolStatus) {
	o.key(entry.Name)
	o.open()
	o.str("name", entry.Name)
	o.str("description", entry.Spec.Description)
	schema := entry.Spec.Schema
	if len(schema) == 0 {
		schema = jsontext.Value(`{"type":"object"}`)
	}
	o.raw("inputSchema", schema)
	o.close()
}
