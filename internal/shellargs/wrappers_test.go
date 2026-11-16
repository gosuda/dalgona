package shellargs

import (
	"slices"
	"testing"
)

func mustTokens(t *testing.T, command string) []string {
	t.Helper()
	tokens, ok := Tokens(command)
	if !ok {
		t.Fatalf("Tokens(%q) rejected the fixture", command)
	}
	return tokens
}

func TestStripWrappers(t *testing.T) {
	rows := []struct {
		command string
		want    string
	}{
		{"kill 4242", "kill 4242"},
		{"echo env kill 4242", "echo env kill 4242"},
		{"env A=1 B=2 kill 4242", "kill 4242"},
		{"/usr/bin/env -i -u HOME -C /tmp kill 4242", "kill 4242"},
		{"env -0 --ignore-environment --unset=HOME --chdir=/tmp kill 4242", "kill 4242"},
		{"sudo -u root -E kill 4242", "kill 4242"},
		{"sudo -nu root kill 4242", "kill 4242"},
		{"sudo -uroot kill 4242", "kill 4242"},
		{"doas -u root kill 4242", "kill 4242"},
		{"sudo -- kill 4242", "kill 4242"},
		{"timeout -s KILL 10 kill 4242", "kill 4242"},
		{"timeout -k 5 1.5m kill 4242", "kill 4242"},
		{"timeout --kill-after=5 --signal=TERM --foreground --preserve-status -v 10s kill 4242", "kill 4242"},
		{"nice -n 5 kill 4242", "kill 4242"},
		{"nice -n5 kill 4242", "kill 4242"},
		{"nice -10 kill 4242", "kill 4242"},
		{"nice --adjustment=5 kill 4242", "kill 4242"},
		{"nohup time -p command -p exec -a name -c -l kill 4242", "kill 4242"},
		{"builtin kill 4242", "kill 4242"},
		{"watch npx bunx pnpx tsc", "tsc"},
		{"A=1 sudo -- B=2 kill 4242", "kill 4242"},
		{"then kill 4242", "kill 4242"},
		{"! kill 4242", "kill 4242"},
		{"do sudo kill 4242", "kill 4242"},
		{`\ sudo kill 4242`, "kill 4242"},
		{"sudo", ""},
		{"A=1", ""},
		{"env -S 'kill 4242'", "env -S 'kill 4242'"},
		{"env -S", "env -S"},
		{"sudo -i kill 4242", "sudo -i kill 4242"},
		{"sudo -s kill 4242", "sudo -s kill 4242"},
		{"sudo -l", "sudo -l"},
		{"sudo -u", "sudo -u"},
		{"sudo -g wheel kill 4242", "sudo -g wheel kill 4242"},
		{"timeout soon kill 4242", "timeout soon kill 4242"},
		{"timeout kill 4242", "timeout kill 4242"},
		{"timeout 10", ""},
		{"timeout --signal TERM 10 kill 4242", "timeout --signal TERM 10 kill 4242"},
		{"nice -n", "nice -n"},
		{"time -f %e kill 4242", "time -f %e kill 4242"},
		{"command -v kill", "command -v kill"},
		{"env A=1 sudo -i kill 4242", "sudo -i kill 4242"},
		{"nohup env -S x kill 4242", "env -S x kill 4242"},
	}
	for _, row := range rows {
		t.Run(row.command, func(t *testing.T) {
			tokens := mustTokens(t, row.command)
			got := StripWrappers(tokens)
			want := mustTokens(t, row.want)
			if !slices.Equal(got, want) {
				t.Fatalf("StripWrappers(%q) = %q, want %q", tokens, got, want)
			}
			if len(got) > 0 && &got[0] != &tokens[len(tokens)-len(got)] {
				t.Fatalf("StripWrappers(%q) = %q is not a suffix of the input", tokens, got)
			}
		})
	}
}

func TestIsWrapper(t *testing.T) {
	for _, token := range []string{"env", "/usr/bin/env", " sudo", "doas", "timeout", "nice", "nohup", "time", "command", "builtin", "exec", "watch", "npx", "bunx", "pnpx"} {
		if !IsWrapper(token) {
			t.Errorf("IsWrapper(%q) = false, want true", token)
		}
	}
	for _, token := range []string{"kill", "echo", "", "environ", "then", "A=1", "sudoedit"} {
		if IsWrapper(token) {
			t.Errorf("IsWrapper(%q) = true, want false", token)
		}
	}
}
