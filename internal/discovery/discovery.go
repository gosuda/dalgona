package discovery

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

type (
	Instruction struct{ Path, Text string }
	MCPServer   struct {
		Name, Command string
		Args, Env     []string
		URL           string
		Workspace     bool
	}
	Skill  struct{ Name, Description, Path string }
	Result struct {
		Instructions []Instruction
		MCPServers   []MCPServer
		Skills       []Skill
	}
	Native struct {
		Instructions []Instruction
		MCPServers   []MCPServer
		Skills       []Skill
	}
	merger struct {
		result Result
		mcp    map[string]struct{}
		skills map[string]struct{}
	}
)

func Discover(home, cwd string, sources []string, n Native) (Result, error) {
	enabled, err := enabledSources(sources)
	if err != nil {
		return Result{}, err
	}
	m := newMerger(n)
	for _, source := range []string{"claude", "codex", "claude-plugins"} {
		if !enabled[source] {
			continue
		}
		r, err := loadSource(source, home, cwd)
		if err != nil {
			return Result{}, fmt.Errorf("discovery: %s: %w", source, err)
		}
		m.add(r)
	}
	return m.result, nil
}

func enabledSources(sources []string) (map[string]bool, error) {
	enabled := make(map[string]bool, len(sources))
	for _, source := range sources {
		switch source {
		case "claude", "codex", "claude-plugins":
			enabled[source] = true
		default:
			return nil, fmt.Errorf("discovery: unknown source %q", source)
		}
	}
	return enabled, nil
}

func loadSource(source, home, cwd string) (Result, error) {
	switch source {
	case "claude":
		return claude(home, cwd)
	case "codex":
		return codex(home, cwd)
	case "claude-plugins":
		return plugins(home)
	default:
		return Result{}, fmt.Errorf("unknown source %q", source)
	}
}

func newMerger(n Native) merger {
	m := merger{
		result: Result{
			Instructions: slices.Clone(n.Instructions),
			MCPServers:   slices.Clone(n.MCPServers),
			Skills:       slices.Clone(n.Skills),
		},
		mcp:    make(map[string]struct{}, len(n.MCPServers)),
		skills: make(map[string]struct{}, len(n.Skills)),
	}
	for _, server := range n.MCPServers {
		m.mcp[server.Name] = struct{}{}
	}
	for _, skill := range n.Skills {
		m.skills[skill.Name] = struct{}{}
	}
	return m
}

func (m *merger) add(r Result) {
	m.result.Instructions = append(m.result.Instructions, r.Instructions...)
	for _, server := range r.MCPServers {
		if _, exists := m.mcp[server.Name]; exists {
			continue
		}
		m.mcp[server.Name] = struct{}{}
		m.result.MCPServers = append(m.result.MCPServers, server)
	}
	for _, skill := range r.Skills {
		if _, exists := m.skills[skill.Name]; exists {
			continue
		}
		m.skills[skill.Name] = struct{}{}
		m.result.Skills = append(m.result.Skills, skill)
	}
}

func codex(home, cwd string) (Result, error) {
	r, err := tree(home, cwd, ".codex", "AGENTS.md")
	if err != nil {
		return Result{}, err
	}
	roots := []struct {
		path      string
		workspace bool
	}{
		{filepath.Join(home, ".codex"), false},
		{filepath.Join(cwd, ".codex"), true},
	}
	for _, root := range roots {
		servers, err := tomlMCP(filepath.Join(root.path, "config.toml"))
		if err != nil {
			return Result{}, err
		}
		if root.workspace {
			markWorkspaceServers(servers)
		}
		r.MCPServers = append(r.MCPServers, servers...)
	}
	return r, nil
}

func claude(home, cwd string) (Result, error) {
	r, err := tree(home, cwd, ".claude", "CLAUDE.md")
	if err != nil {
		return Result{}, err
	}
	paths := []struct {
		path      string
		workspace bool
	}{
		{filepath.Join(home, ".claude.json"), false},
		{filepath.Join(home, ".claude", "mcp.json"), false},
		{filepath.Join(cwd, ".claude", "mcp.json"), true},
		{filepath.Join(cwd, ".mcp.json"), true},
	}
	for _, item := range paths {
		servers, err := jsonMCP(item.path)
		if err != nil {
			return Result{}, err
		}
		if item.workspace {
			markWorkspaceServers(servers)
		}
		r.MCPServers = append(r.MCPServers, servers...)
	}
	return r, nil
}

func markWorkspaceServers(servers []MCPServer) {
	for i := range servers {
		servers[i].Workspace = true
	}
}

func tree(home, cwd, dir, instruction string) (Result, error) {
	r := Result{}
	for _, root := range []string{filepath.Join(home, dir), filepath.Join(cwd, dir)} {
		item, err := readInstruction(root, filepath.Join(root, instruction))
		if err != nil {
			return Result{}, err
		}
		if item != nil {
			r.Instructions = append(r.Instructions, *item)
		}
		found, err := skillDirs(filepath.Join(root, "skills"))
		if err != nil {
			return Result{}, err
		}
		r.Skills = append(r.Skills, found...)
	}
	return r, nil
}

func readInstruction(root, path string) (*Instruction, error) {
	resolved, inside, err := resolveWithin(root, path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("resolve instruction %s: %w", path, err)
	}
	if !inside {
		return nil, nil
	}
	data, err := os.ReadFile(resolved)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read instruction %s: %w", path, err)
	}
	return &Instruction{Path: path, Text: string(data)}, nil
}

func resolveWithin(root, target string) (string, bool, error) {
	base, err := filepath.EvalSymlinks(root)
	if err != nil {
		return "", false, err
	}
	resolved, err := filepath.EvalSymlinks(target)
	if err != nil {
		return "", false, err
	}
	relative, err := filepath.Rel(base, resolved)
	if err != nil {
		return "", false, err
	}
	inside := relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator))
	return resolved, inside, nil
}
