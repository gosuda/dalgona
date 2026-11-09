//go:build unix

package main

import (
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/config"
)

func fakeHerdr(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	log := filepath.Join(dir, "argv.log")
	script := "#!/bin/sh\nprintf '%s\\n' \"$*\" >> " + log + "\n" +
		"if [ \"$2\" = split ]; then printf '{\"result\":{\"pane\":{\"pane_id\":\"pane-child\"}}}'; fi\n"
	if err := os.WriteFile(filepath.Join(dir, "herdr"), []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	return log
}

func herdrEnv(key string) string {
	return map[string]string{"HERDR_ENV": "1", "HERDR_PANE_ID": "pane-main"}[key]
}

func TestHerdrReportsStatusChildrenAndRelease(t *testing.T) {
	log := fakeHerdr(t)
	var env app.CLIEnv
	release := attachHerdr(t.Context(), &env, config.HerdrConfig{SubagentPanes: true}, herdrEnv)
	for _, status := range []app.Status{
		{State: "idle"},
		{State: "running", Seq: 3, ThreadID: "thread-a"},
		{State: "awaiting approval", Seq: 7, ThreadID: "thread-a"},
		{State: "running", Seq: 9, ThreadID: "thread-a"},
		{State: "running", Seq: 9, ThreadID: "thread-a"},
		{State: "idle", Seq: 4, ThreadID: "thread-a"},
		{State: "idle", Seq: 2, ThreadID: "thread-b"},
	} {
		env.OnStatus(status)
	}
	env.OnChildSpawned("thread-child", "/work")
	release()
	data, err := os.ReadFile(log)
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(strings.TrimSpace(string(data)), "\n")
	report := func(state, seq, thread string) string {
		return "pane report-agent pane-main --source dalgona --agent dalgona --state " + state + " --seq " + seq +
			" --agent-session-id " + thread + " -- dalgona --resume " + thread
	}
	allowed := map[string]bool{
		report("working", "3", "thread-a"): true, report("blocked", "7", "thread-a"): true,
		report("working", "9", "thread-a"): true, report("idle", "2", "thread-b"): true,
		"pane split --current --direction right --cwd /work --no-focus": true,
		"pane run pane-child dalgona --resume thread-child":             true,
	}
	for _, line := range got[:len(got)-1] {
		if !allowed[line] {
			t.Fatalf("unexpected herdr call %q in:\n%s", line, data)
		}
	}
	for _, want := range []string{report("working", "9", "thread-a"), report("idle", "2", "thread-b"), "pane run pane-child dalgona --resume thread-child"} {
		if !slices.Contains(got, want) {
			t.Fatalf("herdr calls lack %q:\n%s", want, data)
		}
	}
	if last := got[len(got)-1]; last != "pane release-agent pane-main --source dalgona --agent dalgona" {
		t.Fatalf("last herdr call = %q, want the release", last)
	}
	requireMonotonicSeq(t, got)
}

func requireMonotonicSeq(t *testing.T, lines []string) {
	t.Helper()
	last := map[string]uint64{}
	for _, line := range lines {
		fields := strings.Fields(line)
		if len(fields) < 13 || fields[1] != "report-agent" {
			continue
		}
		seq, err := strconv.ParseUint(fields[10], 10, 64)
		if err != nil {
			t.Fatal(err)
		}
		thread := fields[12]
		if seq < last[thread] {
			t.Fatalf("seq for %s went from %d to %d", thread, last[thread], seq)
		}
		last[thread] = seq
	}
}

func TestHerdrResetsSequenceWhenTheThreadChanges(t *testing.T) {
	log := fakeHerdr(t)
	var env app.CLIEnv
	release := attachHerdr(t.Context(), &env, config.HerdrConfig{}, herdrEnv)
	env.OnStatus(app.Status{State: "running", Seq: 9, ThreadID: "thread-a"})
	waitForCalls(t, log, 1)
	env.OnStatus(app.Status{State: "idle", Seq: 2, ThreadID: "thread-b"})
	waitForCalls(t, log, 2)
	env.OnStatus(app.Status{State: "working", Seq: 3, ThreadID: "thread-a"})
	release()
	data, err := os.ReadFile(log)
	if err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimSpace(string(data)), "\n")
	if len(lines) != 4 || !strings.Contains(lines[2], "--seq 3 --agent-session-id thread-a") {
		t.Fatalf("herdr calls after switching back to thread-a:\n%s", data)
	}
}

func waitForCalls(t *testing.T, log string, want int) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		data, err := os.ReadFile(log)
		if err == nil && strings.Count(string(data), "\n") >= want {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("herdr never received %d calls", want)
}

func TestHerdrSubagentPanesOffSkipsSplit(t *testing.T) {
	fakeHerdr(t)
	var env app.CLIEnv
	release := attachHerdr(t.Context(), &env, config.HerdrConfig{}, herdrEnv)
	defer release()
	if env.OnChildSpawned != nil {
		t.Fatal("child panes wired although [herdr] subagent_panes is false")
	}
	if env.OnStatus == nil {
		t.Fatal("status reports not wired inside herdr")
	}
}

func TestHerdrOutsideHerdrWiresNothing(t *testing.T) {
	var env app.CLIEnv
	release := attachHerdr(t.Context(), &env, config.HerdrConfig{SubagentPanes: true}, func(string) string { return "" })
	release()
	if env.OnStatus != nil || env.OnChildSpawned != nil {
		t.Fatal("herdr hooks wired outside herdr")
	}
}
