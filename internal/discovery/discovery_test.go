package discovery_test

import (
	"encoding/json/v2"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/discovery"
)

func TestDiscoverDisabledByDefaultAndRejectsUnknownSource(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	writeFixture(t, filepath.Join(home, ".codex", "config.toml"), "[mcp_servers")
	writeFixture(t, filepath.Join(home, ".claude.json"), "{")
	writeFixture(t, filepath.Join(home, ".claude", "plugins", "installed_plugins.json"), "{")
	native := discovery.Native{
		Instructions: []discovery.Instruction{{Path: "native.md", Text: "native"}},
		MCPServers:   []discovery.MCPServer{{Name: "native", Command: "run"}},
		Skills:       []discovery.Skill{{Name: "native", Description: "built in"}},
	}
	got, err := discovery.Discover(home, cwd, nil, native)
	if err != nil {
		t.Fatalf("disabled sources were read: %v", err)
	}
	want := discovery.Result(native)
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %#v, want native result %#v", got, want)
	}
	_, err = discovery.Discover(home, cwd, []string{"unknown"}, discovery.Native{})
	if err == nil || err.Error() != `discovery: unknown source "unknown"` {
		t.Fatalf("got error %v, want unknown source", err)
	}
}

func TestDiscoverSourcesAndPrecedence(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	native := discovery.Native{
		Instructions: []discovery.Instruction{{Path: "native.md", Text: "native"}},
		MCPServers:   []discovery.MCPServer{{Name: "shared", Command: "native"}},
		Skills:       []discovery.Skill{{Name: "native-skill", Description: "native"}},
	}
	writeFixture(t, filepath.Join(home, ".claude", "CLAUDE.md"), "home claude")
	writeFixture(t, filepath.Join(cwd, ".claude", "CLAUDE.md"), "project claude")
	writeJSON(t, filepath.Join(home, ".claude.json"), map[string]any{
		"mcpServers": map[string]any{
			"shared":  map[string]any{"command": "claude-shared"},
			"ordered": map[string]any{"command": "claude"},
		},
	})
	writeJSON(t, filepath.Join(cwd, ".mcp.json"), map[string]any{
		"mcpServers": map[string]any{"project-only": map[string]any{"command": "claude-project"}},
	})
	writeSkill(t, filepath.Join(home, ".claude", "skills", "claude-only"), "claude-only", "claude")
	writeSkill(t, filepath.Join(home, ".claude", "skills", "ordered"), "ordered", "claude")

	writeFixture(t, filepath.Join(home, ".codex", "AGENTS.md"), "home codex")
	writeFixture(t, filepath.Join(cwd, ".codex", "AGENTS.md"), "project codex")
	writeFixture(t, filepath.Join(home, ".codex", "config.toml"), `
[mcp_servers.ordered]
command = "codex"
[mcp_servers.codex-only]
command = "codex-only"
`)
	writeFixture(t, filepath.Join(cwd, ".codex", "config.toml"), `
[mcp_servers.codex-project]
command = "codex-project"
`)
	writeSkill(t, filepath.Join(home, ".codex", "skills", "codex-only"), "codex-only", "codex")
	writeSkill(t, filepath.Join(home, ".codex", "skills", "ordered"), "ordered", "codex")

	plugin := filepath.Join(home, "plugins", "plug")
	writeJSON(t, filepath.Join(plugin, ".claude-plugin", "plugin.json"), map[string]any{
		"mcpServers": map[string]any{
			"ordered":   map[string]any{"command": "plugin-ordered"},
			"plug-only": map[string]any{"command": "plugin-only"},
		},
	})
	writeSkill(t, filepath.Join(plugin, "skills", "ordered"), "ordered", "plugin")
	writeSkill(t, filepath.Join(plugin, "skills", "plug-only"), "plug-only", "plugin")
	writeJSON(t, filepath.Join(home, ".claude", "plugins", "installed_plugins.json"), map[string]any{
		"version": 2,
		"plugins": map[string]any{
			"plug@market": []any{map[string]any{"installPath": plugin}},
		},
	})

	got, err := discovery.Discover(home, cwd, []string{"claude-plugins", "codex", "claude"}, native)
	if err != nil {
		t.Fatal(err)
	}
	instructionText := make([]string, 0, len(got.Instructions))
	for _, item := range got.Instructions {
		instructionText = append(instructionText, item.Text)
	}
	wantInstructions := []string{"native", "home claude", "project claude", "home codex", "project codex"}
	if !reflect.DeepEqual(instructionText, wantInstructions) {
		t.Fatalf("instructions %v, want %v", instructionText, wantInstructions)
	}
	wantServers := []discovery.MCPServer{
		{Name: "shared", Command: "native"},
		{Name: "ordered", Command: "claude"},
		{Name: "project-only", Command: "claude-project", Workspace: true},
		{Name: "codex-only", Command: "codex-only"},
		{Name: "codex-project", Command: "codex-project", Workspace: true},
		{Name: "plug:ordered", Command: "plugin-ordered"},
		{Name: "plug:plug-only", Command: "plugin-only"},
	}
	if !reflect.DeepEqual(got.MCPServers, wantServers) {
		t.Fatalf("servers %#v, want %#v", got.MCPServers, wantServers)
	}
	wantSkills := []discovery.Skill{
		{Name: "native-skill", Description: "native"},
		{Name: "claude-only", Description: "claude", Path: filepath.Join(home, ".claude", "skills", "claude-only", "SKILL.md")},
		{Name: "ordered", Description: "claude", Path: filepath.Join(home, ".claude", "skills", "ordered", "SKILL.md")},
		{Name: "codex-only", Description: "codex", Path: filepath.Join(home, ".codex", "skills", "codex-only", "SKILL.md")},
		{Name: "plug-only", Description: "plugin", Path: filepath.Join(plugin, "skills", "plug-only", "SKILL.md")},
	}
	if !reflect.DeepEqual(got.Skills, wantSkills) {
		t.Fatalf("skills %#v, want %#v", got.Skills, wantSkills)
	}
}

func TestDiscoverPluginManifestFormsAndEnablement(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	inline := filepath.Join(home, "plugins", "inline")
	pointer := filepath.Join(home, "plugins", "pointer")
	fallback := filepath.Join(home, "plugins", "fallback")
	registryOff := filepath.Join(home, "plugins", "registry-off")
	settingsOff := filepath.Join(home, "plugins", "settings-off")

	writeJSON(t, filepath.Join(inline, ".claude-plugin", "plugin.json"), map[string]any{
		"skills": []any{"components", 42},
		"mcpServers": map[string]any{
			"stdio": map[string]any{
				"command": "${CLAUDE_PLUGIN_ROOT}/bin/run",
				"args":    []string{"${CLAUDE_PLUGIN_ROOT}/asset"},
				"env":     map[string]string{"B": "2", "A": "${CLAUDE_PLUGIN_ROOT}"},
			},
			"http": map[string]any{"url": "https://example.test/mcp"},
		},
	})
	writeSkill(t, filepath.Join(inline, "components", "inline"), "inline-skill", "inline")
	writeJSON(t, filepath.Join(pointer, ".claude-plugin", "plugin.json"), map[string]any{
		"skills":     "skillset",
		"mcpServers": "config/servers.json",
	})
	writeJSON(t, filepath.Join(pointer, "config", "servers.json"), map[string]any{
		"mcpServers": map[string]any{"file": map[string]any{"command": "from-file"}},
	})
	writeSkill(t, filepath.Join(pointer, "skillset", "pointer"), "pointer-skill", "pointer")
	writeJSON(t, filepath.Join(fallback, ".mcp.json"), map[string]any{
		"plain": map[string]any{"command": "fallback"},
	})
	writeSkill(t, filepath.Join(fallback, "skills", "fallback"), "fallback-skill", "fallback")
	writeFixture(t, filepath.Join(registryOff, ".claude-plugin", "plugin.json"), "not json")
	writeFixture(t, filepath.Join(settingsOff, ".claude-plugin", "plugin.json"), "not json")

	writeJSON(t, filepath.Join(home, ".claude", "plugins", "installed_plugins.json"), map[string]any{
		"version": 2,
		"plugins": map[string]any{
			"inline@market":       []any{map[string]any{"installPath": inline}},
			"pointer@market":      []any{map[string]any{"installPath": pointer, "enabled": true}},
			"fallback@market":     []any{map[string]any{"installPath": fallback}},
			"registry-off@market": []any{map[string]any{"installPath": registryOff, "enabled": false}},
			"settings-off@market": []any{map[string]any{"installPath": settingsOff}},
		},
	})
	writeJSON(t, filepath.Join(home, ".claude", "settings.json"), map[string]any{
		"enabledPlugins": map[string]any{
			"settings-off@market": false,
			"inline@market":       "not-a-boolean",
		},
	})

	got, err := discovery.Discover(home, cwd, []string{"claude-plugins"}, discovery.Native{})
	if err != nil {
		t.Fatal(err)
	}
	wantServers := []discovery.MCPServer{
		{Name: "fallback:plain", Command: "fallback"},
		{Name: "inline:http", URL: "https://example.test/mcp"},
		{Name: "inline:stdio", Command: inline + "/bin/run", Args: []string{inline + "/asset"}, Env: []string{"A=" + inline, "B=2"}},
		{Name: "pointer:file", Command: "from-file"},
	}
	if !reflect.DeepEqual(got.MCPServers, wantServers) {
		t.Fatalf("servers %#v, want %#v", got.MCPServers, wantServers)
	}
	wantSkills := []discovery.Skill{
		{Name: "fallback-skill", Description: "fallback", Path: filepath.Join(fallback, "skills", "fallback", "SKILL.md")},
		{Name: "inline-skill", Description: "inline", Path: filepath.Join(inline, "components", "inline", "SKILL.md")},
		{Name: "pointer-skill", Description: "pointer", Path: filepath.Join(pointer, "skillset", "pointer", "SKILL.md")},
	}
	if !reflect.DeepEqual(got.Skills, wantSkills) {
		t.Fatalf("skills %#v, want %#v", got.Skills, wantSkills)
	}
}

func TestDiscoverDeclaredPluginMCPFileMustExist(t *testing.T) {
	home := t.TempDir()
	plugin := filepath.Join(home, "plugins", "broken")
	writeJSON(t, filepath.Join(plugin, ".claude-plugin", "plugin.json"), map[string]any{
		"mcpServers": "missing.json",
	})
	writePluginRegistry(t, home, "broken@market", plugin, nil)
	_, err := discovery.Discover(home, t.TempDir(), []string{"claude-plugins"}, discovery.Native{})
	if !errors.Is(err, os.ErrNotExist) || !strings.Contains(err.Error(), "missing.json") {
		t.Fatalf("got error %v, want missing declared MCP file", err)
	}
}

func TestDiscoverFollowsSkillDirectorySymlink(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	target := filepath.Join(root, "target")
	writeSkill(t, target, "linked", "target")
	link := filepath.Join(home, ".codex", "skills", "linked")
	if err := os.MkdirAll(filepath.Dir(link), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("directory symlinks are unavailable: %v", err)
	}
	got, err := discovery.Discover(home, t.TempDir(), []string{"codex"}, discovery.Native{})
	if err != nil {
		t.Fatal(err)
	}
	want := []discovery.Skill{{Name: "linked", Description: "target", Path: filepath.Join(link, "SKILL.md")}}
	if !reflect.DeepEqual(got.Skills, want) {
		t.Fatalf("got %#v, want linked skill %#v", got.Skills, want)
	}
}

func TestInstructionSymlinkOutsideRootIsIgnored(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	secret := filepath.Join(root, "secret", "AGENTS.md")
	writeFixture(t, secret, "secret")
	link := filepath.Join(home, ".codex", "AGENTS.md")
	if err := os.MkdirAll(filepath.Dir(link), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(secret, link); err != nil {
		t.Skipf("file symlinks are unavailable: %v", err)
	}
	got, err := discovery.Discover(home, t.TempDir(), []string{"codex"}, discovery.Native{})
	if err != nil {
		t.Fatal(err)
	}
	for _, item := range got.Instructions {
		if item.Path == link || item.Text == "secret" {
			t.Fatalf("instruction read through a link outside its root: %#v", item)
		}
	}
}

func TestDiscoverRejectsMalformedSources(t *testing.T) {
	tests := []struct {
		name   string
		source string
		setup  func(t *testing.T, home, cwd string) string
	}{
		{
			name:   "codex TOML",
			source: "codex",
			setup: func(t *testing.T, home, cwd string) string {
				path := filepath.Join(home, ".codex", "config.toml")
				writeFixture(t, path, "[mcp_servers")
				return path
			},
		},
		{
			name:   "claude JSON",
			source: "claude",
			setup: func(t *testing.T, home, cwd string) string {
				path := filepath.Join(home, ".claude.json")
				writeFixture(t, path, "{")
				return path
			},
		},
		{
			name:   "plugin registry",
			source: "claude-plugins",
			setup: func(t *testing.T, home, cwd string) string {
				path := filepath.Join(home, ".claude", "plugins", "installed_plugins.json")
				writeFixture(t, path, "{")
				return path
			},
		},
		{
			name:   "skill frontmatter",
			source: "codex",
			setup: func(t *testing.T, home, cwd string) string {
				path := filepath.Join(home, ".codex", "skills", "bad", "SKILL.md")
				writeFixture(t, path, "---\nname: bad\n")
				return path
			},
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			home := t.TempDir()
			path := test.setup(t, home, t.TempDir())
			_, err := discovery.Discover(home, t.TempDir(), []string{test.source}, discovery.Native{})
			if err == nil || !strings.Contains(err.Error(), path) {
				t.Fatalf("got error %v, want path %s", err, path)
			}
		})
	}
}

func writePluginRegistry(t *testing.T, home, id, root string, enabled *bool) {
	t.Helper()
	entry := map[string]any{"installPath": root}
	if enabled != nil {
		entry["enabled"] = *enabled
	}
	writeJSON(t, filepath.Join(home, ".claude", "plugins", "installed_plugins.json"), map[string]any{
		"version": 2,
		"plugins": map[string]any{id: []any{entry}},
	})
}

func writeSkill(t *testing.T, dir, name, description string) {
	t.Helper()
	writeFixture(t, filepath.Join(dir, "SKILL.md"), "---\nname: "+name+"\ndescription: "+description+"\n---\n")
}

func writeJSON(t *testing.T, path string, value any) {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	writeFixture(t, path, string(data))
}

func writeFixture(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}
