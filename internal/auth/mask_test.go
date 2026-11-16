package auth_test

import (
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
)

var builtinPlaceholder = regexp.MustCompile(`^\{\{secret:builtin\d+:\d+\}\}$`)

type builtinCase struct {
	name   string
	secret string
	index  int
}

func builtinCases() []builtinCase {
	pem := "-----BEGIN " + "RSA PRIVATE KEY-----\n" + strings.Repeat("k", 24) + "\n-----END " + "RSA PRIVATE KEY-----"
	jwt := "eyJ" + strings.Repeat("h", 16) + "." + strings.Repeat("p", 16) + "." + strings.Repeat("s", 16)
	return []builtinCase{
		{"aws", "AKIA" + strings.Repeat("Q", 16), 0},
		{"github", "ghp_" + strings.Repeat("a", 36), 1},
		{"anthropic", "sk-ant-api03-" + strings.Repeat("d", 20), 5},
		{"private key", pem, 7},
		{"jwt", jwt, 8},
	}
}

func awsKey(fill string) string { return "AKIA" + strings.Repeat(fill, 16) }

func newBuiltinStore(t *testing.T, home string) *auth.Store {
	t.Helper()
	store, err := auth.NewWithPatterns(home, nil)
	if err != nil {
		t.Fatal(err)
	}
	return store
}

func requireNoSecretsFile(t *testing.T, home string) {
	t.Helper()
	if _, err := os.Stat(filepath.Join(home, "secrets.json")); !os.IsNotExist(err) {
		t.Fatalf("secrets.json exists or cannot be checked: %v", err)
	}
}

func requireBuiltinRoundTrip(t *testing.T, store *auth.Store, item builtinCase) {
	t.Helper()
	input := "before " + item.secret + " after"
	masked := store.Mask(input)
	placeholder := strings.TrimSuffix(strings.TrimPrefix(masked, "before "), " after")
	if !builtinPlaceholder.MatchString(placeholder) {
		t.Fatalf("%s: masked = %q", item.name, masked)
	}
	if !strings.HasPrefix(placeholder, "{{secret:builtin"+strconv.Itoa(item.index)+":") {
		t.Fatalf("%s: placeholder %q has the wrong rule index, want %d", item.name, placeholder, item.index)
	}
	if again := store.Mask(input); again != masked {
		t.Fatalf("%s: second mask = %q, want %q", item.name, again, masked)
	}
	requireUnmask(t, store, masked, input)
	strict, err := store.UnmaskStrict(masked)
	if err != nil || strict != input {
		t.Fatalf("%s: strict unmask = %q, %v, want %q", item.name, strict, err, input)
	}
}

func TestBuiltinSecretPatterns(t *testing.T) {
	store := newBuiltinStore(t, t.TempDir())
	for _, item := range builtinCases() {
		requireBuiltinRoundTrip(t, store, item)
	}
}

func TestBuiltinPlaceholdersFollowValues(t *testing.T) {
	store := newBuiltinStore(t, t.TempDir())
	first, second := awsKey("Q"), awsKey("R")
	requireMask(t, store, first, "{{secret:builtin0:0}}")
	requireMask(t, store, first, "{{secret:builtin0:0}}")
	requireMask(t, store, second, "{{secret:builtin0:1}}")
	requireMask(t, store, second+" "+first, "{{secret:builtin0:1}} {{secret:builtin0:0}}")
}

func TestBuiltinSecretsSurviveRestart(t *testing.T) {
	home := t.TempDir()
	secret := awsKey("Q")
	input := "key " + secret
	masked := newBuiltinStore(t, home).Mask(input)
	if raw := readMaskSecretsFile(t, home); !strings.Contains(raw, secret) {
		t.Fatalf("secrets.json = %q, want the recorded value", raw)
	}
	requirePrivateMode(t, filepath.Join(home, "secrets.json"))
	reloaded := newBuiltinStore(t, home)
	requireMask(t, reloaded, input, masked)
	requireUnmask(t, reloaded, masked, input)
	strict, err := reloaded.UnmaskStrict(masked)
	if err != nil || strict != input {
		t.Fatalf("strict unmask = %q, %v, want %q", strict, err, input)
	}
}

func TestBuiltinUnrecordedPlaceholderIsRefused(t *testing.T) {
	store := newBuiltinStore(t, t.TempDir())
	const unknown = "{{secret:builtin0:99}}"
	if _, err := store.UnmaskStrict("run " + unknown); err == nil || !strings.Contains(err.Error(), unknown) {
		t.Fatalf("UnmaskStrict error = %v, want one that names %s", err, unknown)
	}
	requireUnmask(t, store, unknown, unknown)
}

func TestBuiltinPatternsIgnoreNearMisses(t *testing.T) {
	home := t.TempDir()
	store := newBuiltinStore(t, home)
	for _, input := range []string{
		"AKIA" + strings.Repeat("Q", 15),
		"sk-" + strings.Repeat("e", 19),
		"ghp_" + strings.Repeat("a", 35),
		"sk-learn-library",
		"sk-machine-learning-library",
		"a.b.c",
	} {
		requireMask(t, store, input, input)
	}
	requireNoSecretsFile(t, home)
}

func TestBuiltinDiscoveryCanBeDisabled(t *testing.T) {
	home := t.TempDir()
	recorded, fresh := awsKey("Q"), awsKey("R")
	masked := newBuiltinStore(t, home).Mask(recorded)
	off, err := auth.NewWithPatterns(home, nil, auth.WithBuiltinPatterns(false))
	if err != nil {
		t.Fatal(err)
	}
	requireMask(t, off, fresh, fresh)
	requireMask(t, off, recorded, masked)
	if raw := readMaskSecretsFile(t, home); strings.Contains(raw, fresh) {
		t.Fatalf("secrets.json recorded %q while discovery was off", fresh)
	}
}

func TestBuiltinPatternsSkipPlaceholderSyntax(t *testing.T) {
	home := t.TempDir()
	store := newBuiltinStore(t, home)
	inside := "{{secret:auth:" + awsKey("Q") + "}}"
	requireMask(t, store, inside, inside)
	requireNoSecretsFile(t, home)
	requireMask(t, store, inside+" "+awsKey("R"), inside+" {{secret:builtin0:0}}")
}

func TestBuiltinBlockCoversNestedKey(t *testing.T) {
	home := t.TempDir()
	store := newBuiltinStore(t, home)
	key := awsKey("Q")
	block := "-----BEGIN " + "RSA PRIVATE KEY-----\nbody " + key + " tail\n-----END " + "RSA PRIVATE KEY-----"
	requireMask(t, store, block, "{{secret:builtin7:0}}")
	if raw := readMaskSecretsFile(t, home); strings.Count(raw, "{{secret:builtin") != 1 {
		t.Fatalf("secrets.json = %q, want one builtin entry", raw)
	}
}

func TestBuiltinPatternsLeaveCredentialValuesToTheirPlaceholder(t *testing.T) {
	home := t.TempDir()
	key := awsKey("Q")
	t.Setenv("DEPLOY_API_KEY", key)
	store := newBuiltinStore(t, home)
	requireMask(t, store, key, "{{secret:DEPLOY_API_KEY}}")
	requireNoSecretsFile(t, home)
}
