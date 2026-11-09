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
