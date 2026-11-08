package discovery

import (
	"bytes"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

type installedPlugin struct {
	InstallPath string `json:"installPath"`
	Enabled     *bool  `json:"enabled"`
}

type pluginManifest struct {
	Skills jsontext.Value `json:"skills"`
	MCP    jsontext.Value `json:"mcpServers"`
}

func plugins(home string) (Result, error) {
	root := filepath.Join(home, ".claude")
	registryPath := filepath.Join(root, "plugins", "installed_plugins.json")
	registry, err := readRegistry(registryPath)
	if errors.Is(err, os.ErrNotExist) {
		return Result{}, nil
	}
	if err != nil {
		return Result{}, err
	}
	disabled, err := readPluginSettings(filepath.Join(root, "settings.json"))
	if err != nil {
		return Result{}, err
	}
	return loadPlugins(registry, disabled)
}

func readRegistry(path string) (map[string][]installedPlugin, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read plugin registry %s: %w", path, err)
	}
	var doc struct {
		Plugins map[string][]installedPlugin `json:"plugins"`
	}
	err = json.Unmarshal(data, &doc)
	if err != nil {
		return nil, fmt.Errorf("parse plugin registry %s: %w", path, err)
	}
	return doc.Plugins, nil
}

func readPluginSettings(path string) (map[string]jsontext.Value, error) {
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read plugin settings %s: %w", path, err)
	}
	var doc struct {
		Enabled map[string]jsontext.Value `json:"enabledPlugins"`
	}
	err = json.Unmarshal(data, &doc)
	if err != nil {
		return nil, fmt.Errorf("parse plugin settings %s: %w", path, err)
	}
	return doc.Enabled, nil
}

func loadPlugins(registry map[string][]installedPlugin, settings map[string]jsontext.Value) (Result, error) {
	out := Result{}
	for _, id := range slices.Sorted(maps.Keys(registry)) {
		name, ok := pluginName(id)
		if !ok || pluginDisabled(id, settings) {
			continue
		}
		found, err := loadPluginEntries(name, id, registry[id])
		if err != nil {
			return Result{}, err
		}
		out.Skills = append(out.Skills, found.Skills...)
		out.MCPServers = append(out.MCPServers, found.MCPServers...)
	}
	return out, nil
}

func pluginDisabled(id string, settings map[string]jsontext.Value) bool {
	value, exists := settings[id]
	return exists && bytes.Equal(bytes.TrimSpace(value), []byte("false"))
}

func loadPluginEntries(name, id string, entries []installedPlugin) (Result, error) {
	out := Result{}
	for _, entry := range entries {
		if entry.InstallPath == "" || (entry.Enabled != nil && !*entry.Enabled) {
			continue
		}
		found, err := pluginRoot(name, entry.InstallPath)
		if err != nil {
			return Result{}, fmt.Errorf("plugin %s: %w", id, err)
		}
		out.Skills = append(out.Skills, found.Skills...)
		out.MCPServers = append(out.MCPServers, found.MCPServers...)
	}
	return out, nil
}

func pluginName(id string) (string, bool) {
	name, marketplace, ok := strings.CutLast(id, "@")
	if name == "" || marketplace == "" {
		return "", false
	}
	return name, ok
}

func pluginRoot(name, root string) (Result, error) {
	manifest, err := readPluginManifest(root)
	if err != nil {
		return Result{}, err
	}
	found, err := pluginSkillDirs(root, manifest.Skills)
	if err != nil {
		return Result{}, err
	}
	servers, err := pluginServerDocs(root, manifest.MCP)
	if err != nil {
		return Result{}, err
	}
	resolvePluginRoots(servers, root)
	return Result{Skills: found, MCPServers: convert(servers, name+":")}, nil
}

func pluginSkillDirs(root string, raw jsontext.Value) ([]Skill, error) {
	configured, err := pluginSkillPaths(raw)
	if err != nil {
		return nil, err
	}
	if len(configured) == 0 {
		configured = []string{"skills"}
	}
	var found []Skill
	for _, path := range configured {
		skillRoot, err := pluginPath(root, path)
		if err != nil {
			return nil, err
		}
		skills, err := skillDirs(skillRoot)
		if err != nil {
			return nil, err
		}
		found = append(found, skills...)
	}
	return found, nil
}

func pluginSkillPaths(raw jsontext.Value) ([]string, error) {
	raw = bytes.TrimSpace(raw)
	if len(raw) == 0 || bytes.Equal(raw, []byte("null")) {
		return nil, nil
	}
	if raw[0] == '[' {
		return pluginSkillPathArray(raw)
	}
	path, err := pluginString(raw)
	if err != nil || path == "" {
		return nil, err
	}
	return []string{path}, nil
}

func pluginSkillPathArray(raw jsontext.Value) ([]string, error) {
	var values []jsontext.Value
	if err := json.Unmarshal(raw, &values); err != nil {
		return nil, fmt.Errorf("parse plugin skills paths: %w", err)
	}
	paths := make([]string, 0, len(values))
	for _, value := range values {
		value = bytes.TrimSpace(value)
		if len(value) == 0 || value[0] != '"' {
			continue
		}
		path, err := pluginString(value)
		if err != nil {
			return nil, err
		}
		if path != "" {
			paths = append(paths, path)
		}
	}
	return paths, nil
}

func pluginString(raw jsontext.Value) (string, error) {
	var value string
	if err := json.Unmarshal(raw, &value); err != nil {
		return "", fmt.Errorf("parse plugin path: %w", err)
	}
	return value, nil
}

func resolvePluginRoots(servers map[string]mcpDoc, root string) {
	replacer := strings.NewReplacer("${CLAUDE_PLUGIN_ROOT}", root, "${OMP_PLUGIN_ROOT}", root)
	for name, doc := range servers {
		doc.Command = replacer.Replace(doc.Command)
		for i := range doc.Args {
			doc.Args[i] = replacer.Replace(doc.Args[i])
		}
		for key, value := range doc.Env {
			doc.Env[key] = replacer.Replace(value)
		}
		servers[name] = doc
	}
}

func readPluginManifest(root string) (pluginManifest, error) {
	path := filepath.Join(root, ".claude-plugin", "plugin.json")
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return pluginManifest{}, nil
	}
	if err != nil {
		return pluginManifest{}, fmt.Errorf("read plugin manifest %s: %w", path, err)
	}
	var manifest pluginManifest
	err = json.Unmarshal(data, &manifest)
	if err != nil {
		return pluginManifest{}, fmt.Errorf("parse plugin manifest %s: %w", path, err)
	}
	return manifest, nil
}

func pluginServerDocs(root string, raw jsontext.Value) (map[string]mcpDoc, error) {
	raw = bytes.TrimSpace(raw)
	if len(raw) == 0 || bytes.Equal(raw, []byte("null")) {
		return optionalPluginServerFile(filepath.Join(root, ".mcp.json"))
	}
	if raw[0] != '"' {
		var servers map[string]mcpDoc
		if err := json.Unmarshal(raw, &servers); err != nil {
			return nil, fmt.Errorf("parse inline plugin MCP servers: %w", err)
		}
		return servers, nil
	}
	var configured string
	if err := json.Unmarshal(raw, &configured); err != nil {
		return nil, fmt.Errorf("parse plugin MCP path: %w", err)
	}
	if configured == "" {
		return optionalPluginServerFile(filepath.Join(root, ".mcp.json"))
	}
	path, err := pluginPath(root, configured)
	if err != nil {
		return nil, err
	}
	return requiredPluginServerFile(path)
}

func optionalPluginServerFile(path string) (map[string]mcpDoc, error) {
	servers, err := readPluginServerFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	return servers, err
}

func requiredPluginServerFile(path string) (map[string]mcpDoc, error) {
	return readPluginServerFile(path)
}

func readPluginServerFile(path string) (map[string]mcpDoc, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read plugin MCP config %s: %w", path, err)
	}
	servers, err := pluginServers(data)
	if err != nil {
		return nil, fmt.Errorf("parse plugin MCP config %s: %w", path, err)
	}
	return servers, nil
}

func pluginPath(root, configured string) (string, error) {
	base, err := filepath.Abs(root)
	if err != nil {
		return "", fmt.Errorf("resolve plugin root %s: %w", root, err)
	}
	target := configured
	if !filepath.IsAbs(target) {
		target = filepath.Join(base, target)
	}
	target, err = filepath.Abs(target)
	if err != nil {
		return "", fmt.Errorf("resolve plugin path %s: %w", configured, err)
	}
	relative, err := filepath.Rel(base, target)
	if err != nil {
		return "", fmt.Errorf("check plugin path %s: %w", configured, err)
	}
	if relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("plugin path %q escapes plugin root %s", configured, root)
	}
	return target, nil
}
