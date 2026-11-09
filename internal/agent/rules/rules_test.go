package rules

import (
	"fmt"
	"math/rand/v2"
	"slices"
	"strings"
	"testing"
)

func TestRiskRules(t *testing.T) {
	cases := []struct{ name, command, near string }{
		{"substitution", `echo $(cat x)`, "echo $ x"},
		{"shell-wrapper", `foo'bar`, "echo x"},
		{"privilege", "sudo ls", "sudoku"},
		{"nested-shell", "bash -c ls", "bashful"},
		{"inline-interpreter", "python -c print(1)", "python script.py"},
		{"recursive-delete", "rm -rf dir", "rm -f file"},
		{"git-destructive", "git push origin main", "git status"},
		{"publish", "npm publish", "npm install"},
		{"deploy", "terraform apply", "terraform plan"},
		{"credentials", "cat .env", "cat env"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			name, _, ok := Check(tc.command)
			if !ok || name != tc.name {
				t.Fatalf("got %q, %v", name, ok)
			}
			if _, _, ok := Check(tc.near); ok {
				t.Fatalf("near miss matched")
			}
		})
	}
}

func TestNormalizedRiskCommands(t *testing.T) {
	cases := []struct{ command, rule string }{
		{"FOO=1 git push origin", "git-destructive"},
		{"/usr/bin/env sudo ls", "privilege"},
		{`\ sudo ls`, "privilege"},
	}
	for _, tc := range cases {
		name, _, ok := Check(tc.command)
		if !ok || name != tc.rule {
			t.Errorf("Check(%q) = %q, %v; want %q", tc.command, name, ok, tc.rule)
		}
	}
}

func TestSplitProperty(t *testing.T) {
	rng := rand.New(rand.NewPCG(1, 2))
	safe := []string{"go test ./...", `printf "%s" "a&&b"`, `echo a\|b`, "git status", `printf 'x;y|z'`}
	operators := []string{"&&", "||", ";", "|"}
	for range 500 {
		parts := make([]string, 1+rng.IntN(8))
		var command strings.Builder
		for i := range parts {
			if i > 0 {
				command.WriteString(" " + operators[rng.IntN(len(operators))] + " ")
			}
			parts[i] = safe[rng.IntN(len(safe))]
			command.WriteString(parts[i])
		}
		if got := Split(command.String()); !slices.Equal(got, parts) {
			t.Fatalf("Split(%q) = %q, want %q", command.String(), got, parts)
		}
	}
}

func TestSubstitutionProperty(t *testing.T) {
	rng := rand.New(rand.NewPCG(3, 4))
	markers := []string{"$(", "`", "<(", ">("}
	for i := range 200 {
		command := fmt.Sprintf("echo x%d %s value", rng.IntN(1_000_000), markers[i%len(markers)])
		name, _, ok := Check(command)
		if !ok || name != "substitution" {
			t.Fatalf("Check(%q) = %q, %v; want substitution", command, name, ok)
		}
	}
}

func TestGrants(t *testing.T) {
	cases := []grantCase{
		{
			name:          "two tokens",
			command:       "go test ./...",
			prefix:        "go test",
			prefixWithout: "./",
			matches:       []string{"go test -run X"},
			rejects:       []string{"go build"},
		},
		{
			name:    "flag second token",
			command: "ls -la",
			prefix:  "ls -la",
			matches: []string{"ls -la", "ls -la /tmp"},
			rejects: []string{"ls src", "ls src && go build"},
		},
		{
			name:    "quoted token boundary",
			command: "echo 'hello world'",
			prefix:  "echo 'hello world'",
			matches: []string{"echo 'hello world'", `echo "hello world"`},
			rejects: []string{"echo hello world"},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) { checkGrantCase(t, tc) })
	}
	if _, ok := GrantPrefix("go test && go vet"); ok {
		t.Fatal("compound grant accepted")
	}
}

type grantCase struct {
	name          string
	command       string
	prefix        string
	prefixWithout string
	matches       []string
	rejects       []string
}

func checkGrantCase(t *testing.T, tc grantCase) {
	t.Helper()
	prefix, ok := GrantPrefix(tc.command)
	if !ok || prefix != tc.prefix {
		t.Fatalf("GrantPrefix(%q) = %q, %v; want %q", tc.command, prefix, ok, tc.prefix)
	}
	if tc.prefixWithout != "" && strings.Contains(prefix, tc.prefixWithout) {
		t.Fatal("argument included in grant prefix")
	}
	for _, command := range tc.matches {
		assertGrantMatch(t, command, prefix)
	}
	for _, command := range tc.rejects {
		assertGrantReject(t, command, prefix)
	}
}

func assertGrantMatch(t *testing.T, command, prefix string) {
	t.Helper()
	checkGrantMatch(t, command, prefix, true)
}

func assertGrantReject(t *testing.T, command, prefix string) {
	t.Helper()
	checkGrantMatch(t, command, prefix, false)
}

func checkGrantMatch(t *testing.T, command, prefix string, want bool) {
	t.Helper()
	if got := MatchesGrant(command, prefix); got != want {
		t.Errorf("MatchesGrant(%q, %q) = %v, want %v", command, prefix, got, want)
	}
}

func TestRecursiveDeleteFlagOrders(t *testing.T) {
	t.Parallel()
	match := []string{
		"rm -rf dir",
		"rm -f -r /tmp/x",
		"rm -v -rf x",
		"rm -r -f x",
		"rm -Rf x",
		"rm --recursive x",
		"rm --force --recursive x",
	}
	for _, command := range match {
		if name, _, ok := Check(command); !ok || name != "recursive-delete" {
			t.Errorf("Check(%q) = %q, %v; want recursive-delete", command, name, ok)
		}
	}
	miss := []string{"rm -f file", "rm file", "rm riffraff", "rm --force file"}
	for _, command := range miss {
		if name, _, ok := Check(command); ok {
			t.Errorf("Check(%q) = %q, %v; want no match", command, name, ok)
		}
	}
}
