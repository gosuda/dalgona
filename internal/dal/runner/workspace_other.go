//go:build !linux && !darwin

package runner

import (
	"crypto/sha256"
	"io"
	"io/fs"
)

const workspaceReadFlag = 0

func fileIdentity(info fs.FileInfo, content func() (io.ReadCloser, error)) []byte {
	if !info.Mode().IsRegular() {
		return nil
	}
	reader, err := content()
	if err != nil {
		return nil
	}
	defer func() { _ = reader.Close() }()
	digest := sha256.New()
	if _, err := io.Copy(digest, reader); err != nil {
		return nil
	}
	return digest.Sum(nil)
}
