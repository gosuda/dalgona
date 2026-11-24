//go:build windows

package runner

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/windows"
)

const (
	capabilitySeedDirectory = "dalgona"
	capabilitySeedFile      = "windows-sandbox-seed"
	capabilitySeedSize      = 32
	readOnlyCapabilityKey   = "read-only"
	rootCapabilityPrefix    = "root:"
)

type rootCapability struct {
	root string
	sid  *windows.SID
}

func capabilitySeed() ([]byte, error) {
	config, err := os.UserConfigDir()
	if err != nil {
		return nil, fmt.Errorf("runner: resolve user config directory: %w", err)
	}
	directory := filepath.Join(config, capabilitySeedDirectory)
	if err := os.MkdirAll(directory, 0o700); err != nil {
		return nil, fmt.Errorf("runner: create capability seed directory: %w", err)
	}
	seed, err := loadSeed(filepath.Join(directory, capabilitySeedFile), capabilitySeedSize)
	if err != nil {
		return nil, fmt.Errorf("runner: capability seed: %w", err)
	}
	return seed, nil
}

func capabilityPathKey(path string) string {
	return strings.ToLower(filepath.Clean(path))
}

func deriveCapabilitySID(seed []byte, key string) string {
	mac := hmac.New(sha256.New, seed)
	_, _ = mac.Write([]byte(key))
	sum := mac.Sum(nil)
	var words [4]uint32
	for index := range words {
		words[index] = max(binary.LittleEndian.Uint32(sum[index*4:index*4+4]), 1)
	}
	return fmt.Sprintf("S-1-5-21-%d-%d-%d-%d", words[0], words[1], words[2], words[3])
}

func capabilitySID(seed []byte, key string) (*windows.SID, error) {
	value := deriveCapabilitySID(seed, key)
	sid, err := windows.StringToSid(value)
	if err != nil {
		return nil, fmt.Errorf("runner: parse capability SID %s: %w", value, err)
	}
	return sid, nil
}

func rootCapabilities(seed []byte, roots []string) ([]rootCapability, error) {
	grants := make([]rootCapability, 0, len(roots))
	for _, root := range roots {
		sid, err := capabilitySID(seed, rootCapabilityPrefix+capabilityPathKey(root))
		if err != nil {
			return nil, err
		}
		grants = append(grants, rootCapability{root: root, sid: sid})
	}
	return grants, nil
}

func restrictingSIDs(readOnly *windows.SID, grants []rootCapability) []*windows.SID {
	sids := make([]*windows.SID, 0, len(grants)+1)
	sids = append(sids, readOnly)
	for _, grant := range grants {
		sids = append(sids, grant.sid)
	}
	return sids
}
