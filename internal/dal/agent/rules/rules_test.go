package rules

import (
	"fmt"
	"math/rand/v2"
	"strings"
	"testing"
)

func TestRiskRules(t *testing.T) {
	cases := []struct{ name, command, near string }{
		{"substitution", `echo $(cat x)`, "echo $ x"},
		{"substitution", "\\\nrm -rf x", `echo a\|b`},
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
				t.Fatalf("Check(%q) = %q, %v", tc.command, name, ok)
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
		{"nohup sudo ls", "privilege"},
		{"command git push", "git-destructive"},
		{"if true; then rm -rf x; fi", "recursive-delete"},
	}
	for _, tc := range cases {
		name, _, ok := Check(tc.command)
		if !ok || name != tc.rule {
			t.Errorf("Check(%q) = %q, %v; want %q", tc.command, name, ok, tc.rule)
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
		{
			name:    "empty quoted second token",
			command: "ls ''",
			prefix:  "ls ''",
			matches: []string{"ls ''", `ls "" /tmp`},
			rejects: []string{"ls", "ls /tmp", "ls -la"},
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
		"rm -rf -- x",
	}
	for _, command := range match {
		if name, _, ok := Check(command); !ok || name != "recursive-delete" {
			t.Errorf("Check(%q) = %q, %v; want recursive-delete", command, name, ok)
		}
	}
	miss := []string{"rm -f file", "rm file", "rm riffraff", "rm --force file", "rm -- -rf"}
	for _, command := range miss {
		if name, _, ok := Check(command); ok {
			t.Errorf("Check(%q) = %q, %v; want no match", command, name, ok)
		}
	}
}

type ruleCase struct{ command, rule string }

func checkRuleCases(t *testing.T, cases []ruleCase) {
	t.Helper()
	for _, tc := range cases {
		name, _, ok := Check(tc.command)
		if name != tc.rule || ok != (tc.rule != "") {
			t.Errorf("Check(%q) = %q, %v; want rule %q", tc.command, name, ok, tc.rule)
		}
	}
}

func TestDenyRuleWrapperStripping(t *testing.T) {
	t.Parallel()
	const rule = "recursive-delete"
	checkRuleCases(t, []ruleCase{
		{"sudo rm -rf /", rule},
		{"doas rm -rf /", rule},
		{"timeout 10s rm -rf /", rule},
		{"nice -n 10 rm -rf /", rule},
		{"nohup rm -rf /", rule},
		{"time -p rm -rf /", rule},
		{"env FOO=1 rm -rf /", rule},
		{"FOO=bar rm -rf /", rule},
		{"builtin exec rm -rf /", rule},
		{"command rm -rf /", rule},
		{"exec rm -rf /", rule},
		{"timeout -s KILL 10 rm -rf /", rule},
		{"timeout --foreground --signal=KILL 1.5m rm -rf /", rule},
		{"nice -19 rm -rf /", rule},
		{"nice --adjustment=5 rm -rf /", rule},
		{"env -i FOO=1 BAR=2 rm -rf /", rule},
		{"env -u HOME rm -rf /", rule},
		{"sudo -u root -E -- rm -rf /", rule},
		{"doas -u root rm -rf /", rule},
		{"exec -a name rm -rf /", rule},
		{"command -p rm -rf /", rule},
		{"sudo env FOO=1 nice -n 5 timeout 10 rm -rf /", rule},
		{"A=1 B=2 /usr/bin/env time -p rm -rf /", rule},
		{"timeout 10s ls -la", ""},
		{"env FOO=1 echo ok", ""},
		{"nice -n 10 go build ./...", ""},
		{"time -p make", ""},
		{"nohup sleep 1", ""},
		{"FOO=bar printf ok", ""},
		{"exec ls", ""},
		{"command echo rm -rf /", ""},
		{"timeout 5 rm -f file", ""},
		{"timeout -s KILL 10 ls -rf", ""},
		{"env -u rm ls", ""},
		{"timeout 10s", ""},
		{"env FOO=1", ""},
		{"nice -n 10", ""},
		{"FOO=bar", ""},
		{"echo sudo rm -rf /", ""},
		{"printf 'timeout 10 rm -rf /'", ""},
	})
}

func TestDenyRuleUnsupportedWrapperForms(t *testing.T) {
	t.Parallel()
	const rule = "shell-wrapper"
	checkRuleCases(t, []ruleCase{
		{"sudo -s rm -rf /", rule},
		{"sudo -i rm -rf /", rule},
		{"doas -s rm -rf /", rule},
		{"env -S 'rm -rf /' ", rule},
		{"timeout --bogus 10 rm -rf /", rule},
		{"timeout abc rm -rf /", rule},
		{"timeout -s", rule},
		{"nice -x rm -rf /", rule},
		{"exec -x rm -rf /", rule},
		{"sudo -u root -E -- rm -rf /", "recursive-delete"},
		{"timeout 10 rm -rf /", "recursive-delete"},
		{"nice -n 10 ls", ""},
	})
}

func TestDenyRulePrivilegeSurvivesStripping(t *testing.T) {
	t.Parallel()
	checkRuleCases(t, []ruleCase{
		{"sudo ls", "privilege"},
		{"doas ls", "privilege"},
		{"env sudo ls", "privilege"},
		{"sudo -u root ls", "privilege"},
		{"sudo env FOO=1 ls", "privilege"},
		{"su -c ls", "privilege"},
		{"sudo git push origin main", "git-destructive"},
		{"sudo rm -rf /", "recursive-delete"},
		{"sudoku", ""},
		{"ls sudo", ""},
		{"echo sudo", ""},
		{"sudo", ""},
	})
}

func TestDenyRuleMalformedQuoteIsShellWrapper(t *testing.T) {
	t.Parallel()
	checkRuleCases(t, []ruleCase{
		{"sudo rm -rf '/", "shell-wrapper"},
		{`timeout 10 rm -rf "/`, "shell-wrapper"},
		{`env FOO=1 rm -rf \`, "shell-wrapper"},
		{"sudo rm -rf '/'", "recursive-delete"},
		{`timeout 10 rm -rf "/"`, "recursive-delete"},
	})
}

func TestDenyRuleNamesTheWrappedPart(t *testing.T) {
	t.Parallel()
	name, part, ok := Check("ls && echo done; sudo timeout 5 rm -rf /")
	if !ok || name != "recursive-delete" || part != "sudo timeout 5 rm -rf /" {
		t.Fatalf("Check = %q, %q, %v; want recursive-delete on the wrapped part", name, part, ok)
	}
	if name, part, ok := Check("ls && echo done; timeout 5 ls /"); ok {
		t.Fatalf("Check = %q, %q, true; want no match", name, part)
	}
}

func TestDenyRuleBareWrapperMatchesNoRule(t *testing.T) {
	t.Parallel()
	checkRuleCases(t, []ruleCase{
		{"env -u .env", ""},
		{"FOO=.env", ""},
		{"env -u .ssh", ""},
		{"sudo -u .aws", ""},
		{"timeout -s .netrc 10", ""},
		{"nice -n 10 FOO=credentials", ""},
		{"env -u .env; ls", ""},
		{"env -u .env && cat .env", "credentials"},
		{"env cat .env", "credentials"},
		{"FOO=.env cat x", "credentials"},
		{"env -C .ssh ls", "credentials"},
		{"cat .env", "credentials"},
	})
}

func TestDenyRuleEmptyQuotedWordKeepsItsPlace(t *testing.T) {
	t.Parallel()
	checkRuleCases(t, []ruleCase{
		{"sudo '' rm -rf /", "shell-wrapper"},
		{`env "" rm -rf /`, "shell-wrapper"},
		{"'' rm -rf /", "shell-wrapper"},
		{"rm '' -rf /", "recursive-delete"},
		{"git push ''", "git-destructive"},
		{"echo ''", ""},
		{"sudo rm -rf /", "recursive-delete"},
	})
}

func TestDenyRuleWrapperChainProperty(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewPCG(5, 6))
	chain := []string{"sudo", "doas -u root", "env A=1", "nice -n 3", "timeout 5s", "nohup", "time -p", "command", "exec", "builtin", "B=2"}
	for range 500 {
		head, privileged := randomWrapperPrefix(rng, chain)
		if name, _, ok := Check(head + "rm -rf /"); !ok || name != "recursive-delete" {
			t.Fatalf("Check(%q) = %q, %v; want recursive-delete", head+"rm -rf /", name, ok)
		}
		name, _, ok := Check(head + "ls -la")
		if ok != privileged || ok && name != "privilege" {
			t.Fatalf("Check(%q) = %q, %v; privileged chain = %v", head+"ls -la", name, ok, privileged)
		}
	}
}

func randomWrapperPrefix(rng *rand.Rand, chain []string) (string, bool) {
	var prefix []string
	privileged := false
	for range rng.IntN(5) {
		link := chain[rng.IntN(len(chain))]
		prefix = append(prefix, link)
		privileged = privileged || strings.HasPrefix(link, "sudo") || strings.HasPrefix(link, "doas")
	}
	return strings.Join(prefix, " ") + " ", privileged
}

func TestCheckRemoteValueStripsWrappers(t *testing.T) {
	t.Parallel()
	cases := []struct {
		value string
		rule  string
	}{
		{"timeout 10 rm -rf /", "recursive-delete"},
		{"nice -n 5 git push origin", "git-destructive"},
		{"sudo -s ls", "privilege"},
		{"it's", ""},
		{"timeout 10 ls", ""},
		{"env -u .env", ""},
		{"env cat .env", "credentials"},
	}
	for _, tc := range cases {
		name, _, ok := CheckRemoteValue(tc.value)
		if name != tc.rule || ok != (tc.rule != "") {
			t.Errorf("CheckRemoteValue(%q) = %q, %v; want rule %q", tc.value, name, ok, tc.rule)
		}
	}
}
