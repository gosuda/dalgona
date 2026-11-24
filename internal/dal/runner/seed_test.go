package runner

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

const testSeedSize = 32

func TestPublishSeedKeepsAnExistingSeed(t *testing.T) {
	path := filepath.Join(t.TempDir(), "seed")
	existing := bytes.Repeat([]byte{7}, testSeedSize)
	if err := os.WriteFile(path, existing, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := publishSeed(path, testSeedSize); err != nil {
		t.Fatal(err)
	}
	seed, err := loadSeed(path, testSeedSize)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(seed, existing) {
		t.Fatalf("seed after a second publish = %x, want the first seed %x", seed, existing)
	}
}

func TestLoadSeedCreatesOneSeedAndReusesIt(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "seed")
	first, err := loadSeed(path, testSeedSize)
	if err != nil {
		t.Fatal(err)
	}
	if len(first) != testSeedSize || bytes.Equal(first, make([]byte, testSeedSize)) {
		t.Fatalf("first seed = %x, want %d random bytes", first, testSeedSize)
	}
	second, err := loadSeed(path, testSeedSize)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(first, second) {
		t.Fatalf("second load = %x, want the first seed %x", second, first)
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Name() != "seed" {
		t.Fatalf("seed directory holds %v, want only the seed file", entries)
	}
}

func TestLoadSeedRefusesSeedOfWrongSize(t *testing.T) {
	path := filepath.Join(t.TempDir(), "seed")
	short := []byte("short")
	if err := os.WriteFile(path, short, 0o600); err != nil {
		t.Fatal(err)
	}
	if seed, err := loadSeed(path, testSeedSize); err == nil {
		t.Fatalf("seed = %x, want an error for a %d-byte seed", seed, len(short))
	}
	kept, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(kept, short) {
		t.Fatalf("seed file = %q after refusal, want it unchanged", kept)
	}
}
