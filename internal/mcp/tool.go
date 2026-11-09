package mcp

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/tool"
)

// source: https://modelcontextprotocol.io/specification/2025-11-25/server/tools
type serverTool struct {
	client *Client
	name   string
	spec   tool.Spec
}

func (c *Client) listToolsPage(ctx context.Context, cursor string) ([]tool.Tool, string, error) {
	params, err := encodeListParams(cursor)
	if err != nil {
		return nil, "", err
	}
	result, err := c.call(ctx, "tools/list", params)
	if err != nil {
		return nil, "", err
	}
	fields, err := decodeObject(result)
	if err != nil {
		return nil, "", fmt.Errorf("mcp: tools/list result: %w", err)
	}
	rawTools, err := decodeArray(fields["tools"])
	if err != nil {
		return nil, "", fmt.Errorf("mcp: tools/list tools: %w", err)
	}
	tools := make([]tool.Tool, 0, len(rawTools))
	for _, rawTool := range rawTools {
		decoded, adaptErr := c.adaptTool(rawTool)
		if adaptErr != nil {
			return nil, "", adaptErr
		}
		tools = append(tools, decoded)
	}
	next := ""
	if rawNext, ok := fields["nextCursor"]; ok {
		next, err = decodeString(rawNext)
		if err != nil {
			return nil, "", fmt.Errorf("mcp: tools/list nextCursor: %w", err)
		}
	}
	return tools, next, nil
}

func (c *Client) adaptTool(raw []byte) (tool.Tool, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return nil, fmt.Errorf("mcp: tool definition: %w", err)
	}
	name, err := decodeString(fields["name"])
	if err != nil || name == "" {
		return nil, errors.New("mcp: tool definition needs a nonempty name")
	}
	description, err := optionalString(fields, "description")
	if err != nil {
		return nil, fmt.Errorf("mcp: tool %s description: %w", name, err)
	}
	schema, ok := fields["inputSchema"]
	if !ok || !schema.IsValid() || schema.Kind() != jsontext.KindBeginObject {
		return nil, fmt.Errorf("mcp: tool %s inputSchema must be an object", name)
	}
	return &serverTool{
		client: c,
		name:   name,
		spec: tool.Spec{
			Name:        c.cfg.Name + "__" + name,
			Description: description,
			Schema:      schema,
		},
	}, nil
}

func (t *serverTool) Spec() tool.Spec {
	return t.spec
}

func (t *serverTool) Class() tool.Class {
	return tool.ClassRemote
}

func (t *serverTool) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	params, err := encodeCallParams(t.name, call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	raw, err := t.client.call(ctx, "tools/call", params)
	if err != nil {
		return tool.Result{}, err
	}
	output, ok, err := decodeToolResult(raw)
	if err != nil {
		return tool.Result{}, fmt.Errorf("mcp: tools/call %s: %w", t.name, err)
	}
	return tool.Result{Output: output, OK: ok}, nil
}

func decodeToolResult(raw []byte) ([]byte, bool, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return nil, false, err
	}
	blocks, err := decodeArray(fields["content"])
	if err != nil {
		return nil, false, fmt.Errorf("content: %w", err)
	}
	var output bytes.Buffer
	for index, rawBlock := range blocks {
		if index > 0 {
			output.WriteByte('\n')
		}
		if blockErr := appendContentBlock(&output, rawBlock); blockErr != nil {
			return nil, false, blockErr
		}
	}
	isError := false
	if rawError, ok := fields["isError"]; ok {
		isError, err = decodeBool(rawError)
		if err != nil {
			return nil, false, fmt.Errorf("isError: %w", err)
		}
	}
	return output.Bytes(), !isError, nil
}

func appendContentBlock(output *bytes.Buffer, raw []byte) error {
	fields, err := decodeObject(raw)
	if err != nil {
		return fmt.Errorf("content block: %w", err)
	}
	kind, err := decodeString(fields["type"])
	if err != nil {
		return fmt.Errorf("content block type: %w", err)
	}
	var text string
	switch kind {
	case "text":
		text, err = decodeString(fields["text"])
	case "image":
		var mimeType string
		mimeType, err = decodeString(fields["mimeType"])
		text = "[image " + mimeType + "]"
	case "resource_link":
		text, err = resourceLinkText(fields)
	default:
		text = "[unsupported content " + kind + "]"
	}
	if err != nil {
		return fmt.Errorf("content block %s: %w", kind, err)
	}
	output.WriteString(text)
	return nil
}

func resourceLinkText(fields map[string]jsontext.Value) (string, error) {
	uri, err := decodeString(fields["uri"])
	if err != nil {
		return "", err
	}
	name, err := optionalString(fields, "name")
	if err != nil {
		return "", err
	}
	if name == "" {
		return "[resource " + uri + "]", nil
	}
	return "[resource " + name + ": " + uri + "]", nil
}

func optionalString(fields map[string]jsontext.Value, name string) (string, error) {
	raw, ok := fields[name]
	if !ok {
		return "", nil
	}
	return decodeString(raw)
}
