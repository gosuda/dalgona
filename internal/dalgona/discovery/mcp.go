package discovery

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"maps"
	"os"
	"slices"

	"github.com/pelletier/go-toml/v2"
)

type mcpDoc struct {
	Command string            `toml:"command" json:"command"`
	Args    []string          `toml:"args" json:"args"`
	Env     map[string]string `toml:"env" json:"env"`
	URL     string            `toml:"url" json:"url"`
}

func tomlMCP(path string) ([]MCPServer, error) {
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read MCP config %s: %w", path, err)
	}
	var doc struct {
		Servers map[string]mcpDoc `toml:"mcp_servers"`
	}
	err = toml.Unmarshal(data, &doc)
	if err != nil {
		return nil, fmt.Errorf("parse MCP config %s: %w", path, err)
	}
	return convert(doc.Servers, ""), nil
}

func jsonMCP(path string) ([]MCPServer, error) {
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read MCP config %s: %w", path, err)
	}
	servers, err := nestedServers(data)
	if err != nil {
		return nil, fmt.Errorf("parse MCP config %s: %w", path, err)
	}
	return convert(servers, ""), nil
}

func nestedServers(data []byte) (map[string]mcpDoc, error) {
	var doc struct {
		Servers map[string]mcpDoc `json:"mcpServers"`
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		return nil, fmt.Errorf("decode mcpServers: %w", err)
	}
	return doc.Servers, nil
}

func pluginServers(data []byte) (map[string]mcpDoc, error) {
	var envelope struct {
		Servers jsontext.Value `json:"mcpServers"`
	}
	if err := json.Unmarshal(data, &envelope); err != nil {
		return nil, fmt.Errorf("decode plugin MCP config: %w", err)
	}
	if len(envelope.Servers) > 0 {
		var servers map[string]mcpDoc
		if err := json.Unmarshal(envelope.Servers, &servers); err != nil {
			return nil, fmt.Errorf("decode mcpServers: %w", err)
		}
		return servers, nil
	}
	var servers map[string]mcpDoc
	if err := json.Unmarshal(data, &servers); err != nil {
		return nil, fmt.Errorf("decode flat plugin MCP config: %w", err)
	}
	return servers, nil
}

func convert(docs map[string]mcpDoc, prefix string) []MCPServer {
	servers := make([]MCPServer, 0, len(docs))
	for _, name := range slices.Sorted(maps.Keys(docs)) {
		doc := docs[name]
		env := environment(doc.Env)
		servers = append(servers, MCPServer{
			Name:    prefix + name,
			Command: doc.Command,
			Args:    slices.Clone(doc.Args),
			Env:     env,
			URL:     doc.URL,
		})
	}
	return servers
}

func environment(values map[string]string) []string {
	if len(values) == 0 {
		return nil
	}
	env := make([]string, 0, len(values))
	for _, key := range slices.Sorted(maps.Keys(values)) {
		env = append(env, key+"="+values[key])
	}
	return env
}
