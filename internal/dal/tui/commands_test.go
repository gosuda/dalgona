package tui

import (
	"context"
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type threadDirCaller struct {
	fakeCaller
	resumeCwds  map[string]string
	resumeErr   error
	execCwds    []string
	searchRoots [][]string
}

func (f *threadDirCaller) Call(ctx context.Context, method string, params, result any) error {
	switch method {
	case "thread/resume":
		return f.callResume(result, params)
	case "thread/read":
		return errors.New("resume must use one coherent snapshot")
	case "thread/start":
		return f.callStart(result)
	case "command/exec":
		return f.callExec(result, params)
	case "fuzzyFileSearch":
		return f.callFileSearch(params)
	}
	return f.fakeCaller.Call(ctx, method, params, result)
}

func (f *threadDirCaller) callResume(result any, params any) error {
	if f.resumeErr != nil {
		return f.resumeErr
	}
	out, ok := result.(*rpc.ThreadResumeResponse)
	if !ok {
		return errors.New("unexpected thread/resume result")
	}
	id := params.(rpc.ThreadResumeParams).ThreadID
	out.Thread = rpc.Thread{ID: id, Name: "resumed"}
	out.Cwd = f.resumeCwds[id]
	out.Model = "local/qwen-test"
	return nil
}

func (f *threadDirCaller) callStart(result any) error {
	out, ok := result.(*rpc.ThreadStartResponse)
	if !ok {
		return errors.New("unexpected thread/start result")
	}
	out.Thread = rpc.Thread{ID: "t9", Name: "started"}
	out.Cwd = "/elsewhere"
	out.Model = "server-model"
	return nil
}

func (f *threadDirCaller) callExec(result any, params any) error {
	f.execCwds = append(f.execCwds, params.(shellParams).Cwd)
	out, ok := result.(*rpc.CommandExecResponse)
	if !ok {
		return errors.New("unexpected command/exec result")
	}
	out.Stdout = "hi\n"
	return nil
}

func (f *threadDirCaller) callFileSearch(params any) error {
	f.searchRoots = append(f.searchRoots, params.(fileSearchParams).Roots)
	return nil
}

func resumedDirModel(t *testing.T, cwd string, cwds map[string]string) (*threadDirCaller, model) {
	t.Helper()
	caller := &threadDirCaller{resumeCwds: cwds}
	m := newTestModel(caller, Options{Cwd: cwd})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	return caller, m
}

func resumeDrive(t *testing.T, m model, id string) model {
	t.Helper()
	m, _ = drive(t, m, m.resumeThread(id)())
	return m
}

func TestShellRunsInResumedThreadDirectory(t *testing.T) {
	t.Run("resumed directory reaches shell and file search", testResumedDirectoryReachesShellAndFileSearch)
	t.Run("folder help and welcome show the supplied directory", testFolderHelpShowsResumedDirectory)
	t.Run("start response directory replaces launch input", testStartResponseDirectoryReplacesLaunchInput)
	t.Run("switch to another resumed directory", testSwitchToAnotherResumedDirectory)
	t.Run("failed resume keeps the previous directory", testFailedResumeKeepsPreviousDirectory)
	t.Run("before a thread opens uses the launch directory", testBeforeThreadUsesLaunchDirectory)
}

func testResumedDirectoryReachesShellAndFileSearch(t *testing.T) {
	caller, m := resumedDirModel(t, "/work", map[string]string{"t1": "/other"})
	m = resumeDrive(t, m, "t1")
	m, command := typeCommand(t, m, "@mod")
	m = runFileSearch(t, m, command)
	if len(caller.searchRoots) != 1 || len(caller.searchRoots[0]) != 1 || caller.searchRoots[0][0] != "/other" {
		t.Fatalf("file search roots = %q, want [[/other]]", caller.searchRoots)
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m.input.composer.SetValue("")
	shellSubmit(t, shellType(t, m, "!pwd"))
	if len(caller.execCwds) != 1 || caller.execCwds[0] != "/other" {
		t.Fatalf("command/exec cwds = %q, want [/other]", caller.execCwds)
	}
}

func testFolderHelpShowsResumedDirectory(t *testing.T) {
	_, m := resumedDirModel(t, "/work", map[string]string{"t1": "/other"})
	if row := bandRowOf(m); strings.Contains(row, "other") {
		t.Fatalf("band shows the resumed directory before a resume: %q", row)
	}
	m = resumeDrive(t, m, "t1")
	if row := bandRowOf(m); !strings.Contains(row, "other") {
		t.Fatalf("band lacks the resumed directory: %q", row)
	}
	m, _ = drive(t, m, press('?', 0, "?"))
	legend := strings.Join(m.helpBody(), "\n")
	if !strings.Contains(legend, "other") || !strings.Contains(legend, "folder") {
		t.Fatalf("help legend lacks the resumed directory: %q", legend)
	}
	welcome := strings.Join(m.welcomeLines(), "\n")
	if !strings.Contains(welcome, "/other") {
		t.Fatalf("welcome lacks the resumed directory: %q", welcome)
	}
}

func testStartResponseDirectoryReplacesLaunchInput(t *testing.T) {
	caller, m := resumedDirModel(t, "/work", nil)
	m, _ = drive(t, m, m.newThread()())
	if got := m.workDir(); got != "/elsewhere" {
		t.Fatalf("workDir after thread/start = %q, want /elsewhere", got)
	}
	shellSubmit(t, shellType(t, m, "!pwd"))
	if len(caller.execCwds) != 1 || caller.execCwds[0] != "/elsewhere" {
		t.Fatalf("command/exec cwds = %q, want [/elsewhere]", caller.execCwds)
	}
}

func testSwitchToAnotherResumedDirectory(t *testing.T) {
	caller, m := resumedDirModel(t, "/work", map[string]string{"t1": "/other", "t2": "/moved"})
	m = resumeDrive(t, m, "t1")
	m = resumeDrive(t, m, "t2")
	shellSubmit(t, shellType(t, m, "!pwd"))
	if len(caller.execCwds) != 1 || caller.execCwds[0] != "/moved" {
		t.Fatalf("command/exec cwds = %q, want [/moved]", caller.execCwds)
	}
}

func testFailedResumeKeepsPreviousDirectory(t *testing.T) {
	caller, m := resumedDirModel(t, "/work", map[string]string{"t1": "/other", "t2": "/moved"})
	m = resumeDrive(t, m, "t1")
	caller.resumeErr = errors.New("resume failed")
	m = resumeDrive(t, m, "t2")
	if got := m.workDir(); got != "/other" {
		t.Fatalf("workDir after failed resume = %q, want /other", got)
	}
	if m.thread.id != "t1" {
		t.Fatalf("thread id after failed resume = %q, want t1", m.thread.id)
	}
	shellSubmit(t, shellType(t, m, "!pwd"))
	if len(caller.execCwds) != 1 || caller.execCwds[0] != "/other" {
		t.Fatalf("command/exec cwds = %q, want [/other]", caller.execCwds)
	}
}

func testBeforeThreadUsesLaunchDirectory(t *testing.T) {
	caller, m := resumedDirModel(t, "/work", map[string]string{"t1": "/other"})
	m, command := typeCommand(t, m, "@mod")
	m = runFileSearch(t, m, command)
	if len(caller.searchRoots) != 1 || len(caller.searchRoots[0]) != 1 || caller.searchRoots[0][0] != "/work" {
		t.Fatalf("file search roots = %q, want [[/work]]", caller.searchRoots)
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m.input.composer.SetValue("")
	m = shellSubmit(t, shellType(t, m, "!pwd"))
	if len(caller.execCwds) != 1 || caller.execCwds[0] != "/work" {
		t.Fatalf("command/exec cwds = %q, want [/work]", caller.execCwds)
	}
	if row := bandRowOf(m); !strings.Contains(row, "work") {
		t.Fatalf("band lacks the launch directory: %q", row)
	}
	if welcome := strings.Join(m.welcomeLines(), "\n"); !strings.Contains(welcome, "/work") {
		t.Fatalf("welcome lacks the launch directory: %q", welcome)
	}
}
