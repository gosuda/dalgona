package battery_test

import (
	"encoding/json/jsontext"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/app/battery"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
)

func buildMCPServer(t *testing.T) string {
	t.Helper()
	name := "mcp-fixture"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	binary := filepath.Join(t.TempDir(), name)
	output, err := exec.CommandContext(t.Context(), "go", "build", "-o", binary, "../../mcp/testdata/server").CombinedOutput()
	if err != nil {
		t.Fatalf("build MCP fixture: %v\n%s", err, output)
	}
	return binary
}

func dalgonaHome(t *testing.T, server string) string {
	t.Helper()
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("HOME", t.TempDir())
	settings := "[mcp_servers.fix]\ncommand = " + strconv.Quote(server) + "\n"
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(settings), 0o600); err != nil {
		t.Fatal(err)
	}
	writeSkill(t, filepath.Join(home, "skills"), "review", "Review a diff.")
	return home
}

func writeSkill(t *testing.T, root, name, description string) string {
	t.Helper()
	dir := filepath.Join(root, name)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "SKILL.md")
	if err := os.WriteFile(path, []byte("---\nname: "+name+"\ndescription: "+description+"\n---\nbody\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestLoadServesConfiguredMCPToolsAndSkills(t *testing.T) {
	dalgonaHome(t, buildMCPServer(t))
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	extra, err := (&battery.Daemon{}).Load(t.Context(), app.Base{Config: cfg})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := extra.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	project := t.TempDir()
	projectSkill := writeSkill(t, filepath.Join(project, ".dalgona", "skills"), "deploy", "Ship a build.")
	requireEcho(t, findTool(t, extra.Tools(project), "fix__echo"))
	listed, err := extra.Skills(project)
	if err != nil {
		t.Fatal(err)
	}
	want := []prompt.Skill{
		{Name: "review", Description: "Review a diff.", Path: filepath.Join(cfg.Home, "skills", "review", "SKILL.md")},
		{Name: "deploy", Description: "Ship a build.", Path: projectSkill},
	}
	if !slices.Equal(listed, want) {
		t.Fatalf("skills = %+v, want %+v", listed, want)
	}
}

func TestNewHostCarriesBatterySkills(t *testing.T) {
	dalgonaHome(t, buildMCPServer(t))
	host, closeHost, err := app.NewHost(t.Context(), app.Options{Product: config.ProductDalgona, Cwd: t.TempDir(), Batteries: (&battery.Daemon{}).Load})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := closeHost(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	listed, err := host.Skills(t.TempDir())
	if err != nil || len(listed) != 1 || listed[0].Name != "review" {
		t.Fatalf("host skills = %+v, %v", listed, err)
	}
}

func TestNativeMCPServersStartOncePerHost(t *testing.T) {
	dalgonaHome(t, buildMCPServer(t))
	startLog := filepath.Join(t.TempDir(), "starts")
	t.Setenv("START_LOG", startLog)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	extra, err := (&battery.Daemon{}).Load(t.Context(), app.Base{Config: cfg})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = extra.Close() })
	for _, cwd := range []string{t.TempDir(), t.TempDir()} {
		findTool(t, extra.Tools(cwd), "fix__echo")
	}
	data, err := os.ReadFile(startLog)
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "start\n" {
		t.Fatalf("native server starts = %q, want one", data)
	}
}

func requireEcho(t *testing.T, echo tool.Tool) {
	t.Helper()
	if echo.Class() != tool.ClassRemote {
		t.Fatalf("fix__echo class = %v", echo.Class())
	}
	result, err := echo.Run(t.Context(), tool.Call{Args: jsontext.Value(`{"text":"hello"}`)}, tool.Env{})
	if err != nil || !result.OK || string(result.Output) != "hello" {
		t.Fatalf("fix__echo = %+v, %v", result, err)
	}
}

func findTool(t *testing.T, tools []tool.Tool, name string) tool.Tool {
	t.Helper()
	for _, candidate := range tools {
		if candidate.Spec().Name == name {
			return candidate
		}
	}
	t.Fatalf("tool %s missing from %d battery tools", name, len(tools))
	return nil
}
