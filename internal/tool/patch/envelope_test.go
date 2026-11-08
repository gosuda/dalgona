package patch

import "testing"

func TestProfileEnvelopePreservesBodyNewline(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "old\n")
	ref := seedReference(env, "f.txt", "old\n", true)
	input := "*** Begin Patch\n" + profileInput("f.txt", ref, "PUT 1:\n+new\n") + "*** End Patch\n"
	if _, err := runPatch(t, env, HashlineLight, input); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "new\n" {
		t.Fatalf("envelope changed body newline: %q", actual)
	}
}
