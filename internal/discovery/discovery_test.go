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
	native := discovery.Result{
		Instructions: []discovery.Instruction{{Path: "native.md", Text: "native"}},
		MCPServers:   []discovery.MCPServer{{Name: "native", Command: "run"}},
		Skills:       []discovery.Skill{{Name: "native", Description: "built in"}},
	}
	got, err := discovery.Discover(home, cwd, nil, native)
	if err != nil {
		t.Fatalf("disabled sources were read: %v", err)
	}
	want := native
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %#v, want native result %#v", got, want)
	}
	_, err = discovery.Discover(home, cwd, []string{"unknown"}, discovery.Result{})
	if err == nil || err.Error() != `discovery: unknown source "unknown"` {
		t.Fatalf("got error %v, want unknown source", err)
	}
}

func TestDiscoverSourcesAndPrecedence(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	native := discovery.Result{
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

	got, err := discovery.Discover(home, cwd, []string{"claude-plugins"}, discovery.Result{})
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
	_, err := discovery.Discover(home, t.TempDir(), []string{"claude-plugins"}, discovery.Result{})
	if !errors.Is(err, os.ErrNotExist) || !strings.Contains(err.Error(), "missing.json") {
		t.Fatalf("got error %v, want missing declared MCP file", err)
	}
}

func TestPluginMCPStringSymlinkOutsideRootIsRejected(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	plugin := filepath.Join(home, "plugins", "linked")
	outside := filepath.Join(root, "outside.json")
	writeJSON(t, outside, map[string]any{"plain": map[string]any{"command": "outside"}})
	writeJSON(t, filepath.Join(plugin, ".claude-plugin", "plugin.json"), map[string]any{
		"mcpServers": "link.json",
	})
	if err := os.Symlink(outside, filepath.Join(plugin, "link.json")); err != nil {
		t.Skipf("file symlinks are unavailable: %v", err)
	}
	writePluginRegistry(t, home, "linked@market", plugin, nil)
	_, err := discovery.Discover(home, t.TempDir(), []string{"claude-plugins"}, discovery.Result{})
	if err == nil || !strings.Contains(err.Error(), `plugin path "link.json" escapes plugin root`) {
		t.Fatalf("got error %v, want plugin path escape", err)
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
	got, err := discovery.Discover(home, t.TempDir(), []string{"codex"}, discovery.Result{})
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
	got, err := discovery.Discover(home, t.TempDir(), []string{"codex"}, discovery.Result{})
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
			_, err := discovery.Discover(home, t.TempDir(), []string{test.source}, discovery.Result{})
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

func TestNativeSkillsFrontmatterFieldsAndPath(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	dir := filepath.Join(home, "skills", "ignored-dir")
	body := "---\r\nname: \"quoted\"\r\ndescription: 'A description'\r\nother: skip\r\n---\r\nbody\r\n"
	path := writeSkillBody(t, dir, body)
	got, err := discovery.NativeSkills(home, cwd, nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 {
		t.Fatalf("got %d skills, want 1", len(got))
	}
	want := discovery.Skill{Name: "quoted", Description: "A description", Path: path}
	if got[0] != want {
		t.Fatalf("got %#v, want %#v", got[0], want)
	}
}

func TestNativeSkillsNameFallsBackToDirectory(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	writeSkillBody(t, filepath.Join(home, "skills", "blank-name"), "---\nname:\n---\n")
	writeSkillBody(t, filepath.Join(home, "skills", "from-dir"), "---\ndescription: only\n---\n")
	writeSkillBody(t, filepath.Join(home, "skills", "quoted-empty"), "---\nname: \"\"\n---\n")
	got, err := discovery.NativeSkills(home, cwd, nil)
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"blank-name", "from-dir", "quoted-empty"}
	if len(got) != len(want) {
		t.Fatalf("got %#v, want names %v", got, want)
	}
	for i := range want {
		if got[i].Name != want[i] {
			t.Fatalf("got names %#v, want %v", got, want)
		}
	}
}

func TestNativeSkillsHomeProjectExtraPrecedence(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	extra := t.TempDir()
	homePath := writeSkillBody(t, filepath.Join(home, "skills", "shared"), "---\nname: shared\n---\n")
	writeSkillBody(t, filepath.Join(cwd, ".dalgona", "skills", "shared"), "---\nname: shared\ndescription: project\n---\n")
	writeSkillBody(t, filepath.Join(extra, "shared"), "---\nname: shared\ndescription: extra\n---\n")
	projectPath := writeSkillBody(t, filepath.Join(cwd, ".dalgona", "skills", "only-project"), "---\nname: only-project\n---\n")
	extraPath := writeSkillBody(t, filepath.Join(extra, "only-extra"), "---\nname: only-extra\n---\n")
	got, err := discovery.NativeSkills(home, cwd, []string{extra})
	if err != nil {
		t.Fatal(err)
	}
	want := []discovery.Skill{
		{Name: "shared", Path: homePath},
		{Name: "only-project", Path: projectPath},
		{Name: "only-extra", Path: extraPath},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %#v, want ordered unique skills %#v", got, want)
	}
}

func TestNativeSkillsIgnoresMissingRootsAndNonDirectories(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	writeFixture(t, filepath.Join(home, "skills"), "file")
	writeFixture(t, filepath.Join(cwd, ".dalgona", "skills"), "file")
	extraFile := filepath.Join(t.TempDir(), "not-a-root")
	writeFixture(t, extraFile, "x")
	extra := t.TempDir()
	writeFixture(t, filepath.Join(extra, "note.txt"), "x")
	if err := os.MkdirAll(filepath.Join(extra, "no-skill"), 0o755); err != nil {
		t.Fatal(err)
	}
	path := writeSkillBody(t, filepath.Join(extra, "real"), "---\nname: real\n---\n")
	got, err := discovery.NativeSkills(home, cwd, []string{extraFile, extra, filepath.Join(home, "missing")})
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].Path != path || got[0].Name != "real" {
		t.Fatalf("got %#v, want only %s", got, path)
	}
}

func TestNativeSkillsFollowsSkillDirectorySymlink(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	target := filepath.Join(root, "target")
	writeSkillBody(t, target, "---\nname: linked\ndescription: target\n---\n")
	link := filepath.Join(home, "skills", "linked")
	if err := os.MkdirAll(filepath.Dir(link), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("directory symlinks are unavailable: %v", err)
	}
	got, err := discovery.NativeSkills(home, t.TempDir(), nil)
	if err != nil {
		t.Fatal(err)
	}
	want := []discovery.Skill{{Name: "linked", Description: "target", Path: filepath.Join(link, "SKILL.md")}}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %#v, want linked skill %#v", got, want)
	}
}

func TestNativeSkillsRejectsMalformedFrontmatter(t *testing.T) {
	openHome := t.TempDir()
	openPath := writeSkillBody(t, filepath.Join(openHome, "skills", "bad-open"), "name: x\n")
	_, err := discovery.NativeSkills(openHome, t.TempDir(), nil)
	assertMalformed(t, err, openPath)

	closeHome := t.TempDir()
	closePath := writeSkillBody(t, filepath.Join(closeHome, "skills", "bad-close"), "---\nname: x\n")
	_, err = discovery.NativeSkills(closeHome, t.TempDir(), nil)
	assertMalformed(t, err, closePath)
}

func assertMalformed(t *testing.T, err error, path string) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), "skills: malformed frontmatter") || !strings.Contains(err.Error(), path) {
		t.Fatalf("got error %v, want malformed frontmatter at %s", err, path)
	}
}

func writeSkillBody(t *testing.T, dir, body string) string {
	t.Helper()
	path := filepath.Join(dir, "SKILL.md")
	writeFixture(t, path, body)
	return path
}
