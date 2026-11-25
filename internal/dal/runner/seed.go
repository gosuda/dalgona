package runner

import (
	"crypto/rand"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
)

func loadSeed(path string, size int) ([]byte, error) {
	seed, err := readSeed(path, size)
	if !errors.Is(err, fs.ErrNotExist) {
		return seed, err
	}
	if err := publishSeed(path, size); err != nil {
		return nil, err
	}
	return readSeed(path, size)
}

func readSeed(path string, size int) ([]byte, error) {
	seed, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read seed %s: %w", path, err)
	}
	if len(seed) != size {
		return nil, fmt.Errorf("seed %s holds %d bytes instead of %d; remove the file to create a new seed", path, len(seed), size)
	}
	return seed, nil
}

func publishSeed(path string, size int) error {
	seed := make([]byte, size)
	if _, err := rand.Read(seed); err != nil {
		return fmt.Errorf("generate seed: %w", err)
	}
	staged, err := os.CreateTemp(filepath.Dir(path), ".dalgona-seed-")
	if err != nil {
		return fmt.Errorf("stage seed: %w", err)
	}
	name := staged.Name()
	defer func() { _ = os.Remove(name) }() //nolint:gosec // the staged path was created by CreateTemp
	_, writeErr := staged.Write(seed)
	syncErr := staged.Sync()
	if err := errors.Join(writeErr, syncErr, staged.Close()); err != nil {
		return fmt.Errorf("stage seed: %w", err)
	}
	if err := os.Link(name, path); err != nil && !errors.Is(err, fs.ErrExist) {
		return fmt.Errorf("publish seed: %w", err)
	}
	return nil
}
