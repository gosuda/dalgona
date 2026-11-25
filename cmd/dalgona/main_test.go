package main

import (
	"bytes"
	"slices"
	"strings"
	"testing"
)

func TestLeadingConfigLiftsOnlyKeyValuePairs(t *testing.T) {
	for _, tc := range []struct {
		args, pairs, rest []string
	}{
		{[]string{"--config", "approval_policy=untrusted", "app-server"}, []string{"--config", "approval_policy=untrusted"}, []string{"app-server"}},
		{[]string{"-c", "model=x", "--config=sandbox_mode=read-only", "app-server", "--listen", "stdio://"}, []string{"-c", "model=x", "--config=sandbox_mode=read-only"}, []string{"app-server", "--listen", "stdio://"}},
		{[]string{"-c", "fix the bug"}, []string{}, []string{"-c", "fix the bug"}},
		{[]string{"-c"}, []string{}, []string{"-c"}},
		{[]string{"app-server"}, []string{}, []string{"app-server"}},
	} {
		pairs, rest := leadingConfig(tc.args)
		if !slices.Equal(pairs, tc.pairs) || !slices.Equal(rest, tc.rest) {
			t.Errorf("leadingConfig(%q) = %q, %q; want %q, %q", tc.args, pairs, rest, tc.pairs, tc.rest)
		}
	}
}

func TestGlobalConfigBeforeAppServerReachesTheAppServer(t *testing.T) {
	t.Setenv("DALGONA_HOME", t.TempDir())
	var stderr bytes.Buffer
	args := []string{"--config", "approval_policy=untrusted", "app-server", "--listen", "bogus://x"}
	code := run(t.Context(), args, stdio{in: strings.NewReader(""), out: &bytes.Buffer{}, errOut: &stderr})
	if code != 2 || !strings.HasPrefix(stderr.String(), "dalgona app-server: ") {
		t.Fatalf("exit %d, stderr %q; want the app-server parser to reject only --listen", code, stderr.String())
	}
}

func TestMachineFlagsOverrideOnlyTheDefaultSelection(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, remote string
		flags        []string
	}{
		{name: "configured default", remote: "default-remote"},
		{name: "explicit local", flags: []string{"--local"}},
		{name: "explicit named endpoint", remote: "another", flags: []string{"--remote", "another"}},
		{name: "empty remote unchanged", remote: "default-remote", flags: []string{"--remote="}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			resume := []string{"--resume", "same-thread"}
			selection, rest, err := splitMachineFlags(append(tc.flags, resume...))
			if err != nil {
				t.Fatal(err)
			}
			if got := selection.remoteName("default-remote"); got != tc.remote || !slices.Equal(rest, resume) {
				t.Fatalf("selected remote %q, remaining args %q; want %q, %q", got, rest, tc.remote, resume)
			}
		})
	}
	for _, args := range [][]string{{"--local", "--remote="}, {"--remote", "another", "--local"}} {
		if code, _, stderr := runCLI(t, args...); code != 2 || !strings.Contains(stderr, "--local cannot be combined with --remote") {
			t.Fatalf("conflicting flags %q: exit %d, stderr %q", args, code, stderr)
		}
	}
}
