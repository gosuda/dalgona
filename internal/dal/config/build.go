package config

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"sync"
)

const buildHexLength = 16

var ownBuild = sync.OnceValue(func() string {
	path, err := os.Executable()
	if err != nil {
		return ""
	}
	build, err := FileBuild(path)
	if err != nil {
		return ""
	}
	return build
})

func Build() string { return ownBuild() }

func FileBuild(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", fmt.Errorf("open build %s: %w", path, err)
	}
	defer file.Close()
	digest := sha256.New()
	if _, err := io.Copy(digest, file); err != nil {
		return "", fmt.Errorf("hash build %s: %w", path, err)
	}
	return hex.EncodeToString(digest.Sum(nil))[:buildHexLength], nil
}
