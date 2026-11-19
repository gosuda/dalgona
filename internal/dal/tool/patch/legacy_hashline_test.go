package patch

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const (
	legacyTagSource   = "one\ntwo\nthree\n"
	unissuedReference = "r00000000000000000000000000000000.1"
)

func legacyHeader(source string) string {
	return "[f.txt#" + snapshot.VersionTag([]byte(source)) + "]\n"
}

func TestLegacyHashlineRemAndMv(t *testing.T) {
	t.Run("COMPAT-01/REM", requireLegacyRem)
	t.Run("COMPAT-01/MV", requireLegacyMv)
	t.Run("COMPAT-01/MV after PUT", requireLegacyMvAfterPut)
	t.Run("COMPAT-01/stale tag", requireLegacyStaleTag)
	t.Run("COMPAT-01/profile reference still uses the store", requireProfileReferenceUsesStore)
}

func requireLegacyRem(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", legacyTagSource)
	if _, err := runPatch(t, env, Hashline, legacyHeader(legacyTagSource)+"REM\n"); err != nil {
		t.Fatalf("REM with the current tag: %v", err)
	}
	requireMissing(t, filepath.Join(cwd, "f.txt"))
}

func requireLegacyMv(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", legacyTagSource)
	if _, err := runPatch(t, env, Hashline, legacyHeader(legacyTagSource)+"MV b.txt\n"); err != nil {
		t.Fatalf("MV with the current tag: %v", err)
	}
	requireMissing(t, filepath.Join(cwd, "f.txt"))
	if got := readFixture(t, cwd, "b.txt"); got != legacyTagSource {
		t.Fatalf("renamed file holds %q, want %q", got, legacyTagSource)
	}
}

func requireLegacyMvAfterPut(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", legacyTagSource)
	input := legacyHeader(legacyTagSource) + "PUT 1:\n+edited\nMV b.txt\n"
	if _, err := runPatch(t, env, Hashline, input); err != nil {
		t.Fatalf("PUT then MV with the current tag: %v", err)
	}
	requireMissing(t, filepath.Join(cwd, "f.txt"))
	if got, want := readFixture(t, cwd, "b.txt"), "edited\ntwo\nthree\n"; got != want {
		t.Fatalf("renamed file holds %q, want %q", got, want)
	}
}

func requireLegacyStaleTag(t *testing.T) {
	const stale = "BEEF"
	current := snapshot.VersionTag([]byte(legacyTagSource))
	if current == stale {
		t.Fatal("fixture tag equals the stale tag")
	}
	want := "patch: changes[0]: tag " + stale + " does not match f.txt now (tag " + current + "). The file changed since you read it; read it again"
	for _, operation := range []string{"REM", "MV b.txt"} {
		env, cwd := editTestEnv(t)
		writeFixture(t, cwd, "f.txt", legacyTagSource)
		_, err := runPatch(t, env, Hashline, "[f.txt#"+stale+"]\n"+operation+"\n")
		if err == nil || err.Error() != want {
			t.Fatalf("%s with a stale tag: error = %v, want %q", operation, err, want)
		}
		if got := readFixture(t, cwd, "f.txt"); got != legacyTagSource {
			t.Fatalf("refused %s changed the file to %q", operation, got)
		}
		requireMissing(t, filepath.Join(cwd, "b.txt"))
	}
}

func requireProfileReferenceUsesStore(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", legacyTagSource)
	_, err := runPatch(t, env, HashlineLight, profileInput("f.txt", unissuedReference, "REM"))
	if err == nil || !strings.Contains(err.Error(), "reference is not eligible") {
		t.Fatalf("REM with a reference the store never issued: error = %v", err)
	}
	if got := readFixture(t, cwd, "f.txt"); got != legacyTagSource {
		t.Fatalf("refused REM changed the file to %q", got)
	}
	ref := seedReference(env, "f.txt", legacyTagSource, true)
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "REM")); err != nil {
		t.Fatalf("REM with an issued reference: %v", err)
	}
	requireMissing(t, filepath.Join(cwd, "f.txt"))
}
