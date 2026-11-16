package patch

import (
	"path/filepath"
	"testing"
)

func TestLocalWritesArePinnedOnEveryPlatform(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "sub/f.txt", "x\n")
	pin, err := pinVerifiedComponents(env, "sub/f.txt", filepath.Join(cwd, "sub", "f.txt"))
	if err != nil {
		t.Fatal(err)
	}
	if pin == nil {
		t.Fatal("a local runner got no directory pin")
	}
	defer pin.close()
	data, err := pin.readFileAt("f.txt")
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "x\n" {
		t.Fatalf("pinned read = %q, want %q", data, "x\n")
	}
}
