package cursor

import (
	"bytes"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

const (
	conversationIDFile = "conversation-id"
	checkpointFile     = "checkpoint"
	blobDir            = "blobs"
)

type conversation struct {
	dir        string
	id         string
	checkpoint []byte
	blobs      map[string][]byte
}

func openConversation(dir string) (*conversation, error) {
	conv := &conversation{dir: dir, blobs: make(map[string][]byte)}
	if dir == "" {
		id, err := randomUUID()
		conv.id = id
		return conv, err
	}
	if err := os.MkdirAll(filepath.Join(dir, blobDir), 0o700); err != nil {
		return nil, fmt.Errorf("cursor: create state dir: %w", err)
	}
	id, err := conv.loadID()
	if err != nil {
		return nil, err
	}
	conv.id = id
	checkpoint, err := os.ReadFile(filepath.Join(dir, checkpointFile))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("cursor: read checkpoint: %w", err)
	}
	conv.checkpoint = checkpoint
	return conv, nil
}

func (c *conversation) loadID() (string, error) {
	path := filepath.Join(c.dir, conversationIDFile)
	raw, err := os.ReadFile(path)
	if err == nil && len(strings.TrimSpace(string(raw))) != 0 {
		return strings.TrimSpace(string(raw)), nil
	}
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return "", fmt.Errorf("cursor: read conversation id: %w", err)
	}
	id, err := randomUUID()
	if err != nil {
		return "", err
	}
	return id, writeAtomic(path, []byte(id))
}

func (c *conversation) storeBlob(data []byte) []byte {
	sum := sha256.Sum256(data)
	c.blobs[hex.EncodeToString(sum[:])] = data
	return sum[:]
}

func (c *conversation) blob(id []byte) []byte {
	key := hex.EncodeToString(id)
	if data, ok := c.blobs[key]; ok {
		return data
	}
	if c.dir == "" {
		return nil
	}
	data, err := os.ReadFile(filepath.Join(c.dir, blobDir, key))
	if err != nil {
		return nil
	}
	c.blobs[key] = data
	return data
}

func (c *conversation) setBlob(id, data []byte) error {
	key := hex.EncodeToString(id)
	owned := bytes.Clone(data)
	c.blobs[key] = owned
	if c.dir == "" {
		return nil
	}
	return writeAtomic(filepath.Join(c.dir, blobDir, key), owned)
}

func (c *conversation) saveCheckpoint(state []byte) error {
	c.checkpoint = append(c.checkpoint[:0], state...)
	if c.dir == "" {
		return nil
	}
	return writeAtomic(filepath.Join(c.dir, checkpointFile), c.checkpoint)
}

func writeAtomic(path string, data []byte) error {
	temp, err := os.CreateTemp(filepath.Dir(path), ".tmp-*")
	if err != nil {
		return fmt.Errorf("cursor: write %s: %w", filepath.Base(path), err)
	}
	name := temp.Name()
	_, writeErr := temp.Write(data)
	closeErr := temp.Close()
	if err := errors.Join(writeErr, closeErr, os.Chmod(name, 0o600)); err != nil {
		_ = os.Remove(name)
		return fmt.Errorf("cursor: write %s: %w", filepath.Base(path), err)
	}
	if err := os.Rename(name, path); err != nil {
		_ = os.Remove(name)
		return fmt.Errorf("cursor: write %s: %w", filepath.Base(path), err)
	}
	return nil
}

func randomUUID() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("cursor: random id: %w", err)
	}
	raw[6] = raw[6]&0x0f | 0x40
	raw[8] = raw[8]&0x3f | 0x80
	hexed := hex.EncodeToString(raw[:])
	return hexed[0:8] + "-" + hexed[8:12] + "-" + hexed[12:16] + "-" + hexed[16:20] + "-" + hexed[20:32], nil
}
