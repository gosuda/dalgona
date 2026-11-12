package search

import (
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"sort"
	"strings"
)

const manifestMagic = "dalgona search manifest 2 codesearch v1.2.0\n"

const (
	manifestHeaderBytes = len(manifestMagic) + 8 + 8 + 4
	manifestTailBytes   = 8 + 8 + 8 + 1
	manifestMinEntry    = 4 + 1 + manifestTailBytes
	manifestMaxPath     = 4096
)

var errManifest = errors.New("search: invalid manifest")

type manifestEntry struct {
	Path    string
	Size    int64
	ModNS   int64
	CtimeNS int64
	Indexed bool
}

type manifest struct {
	Start       int64
	TrigramSize int64
	Entries     []manifestEntry
}

type manifestFrame struct {
	head  manifest
	count int
	body  []byte
}

func badManifest(reason string) error {
	return fmt.Errorf("%w: %s", errManifest, reason)
}

func appendInt64(out []byte, v int64) []byte {
	return binary.BigEndian.AppendUint64(out, uint64(v)) //nolint:gosec // the manifest stores int64 values in two's complement
}

func appendLength(out []byte, n int) []byte {
	return binary.BigEndian.AppendUint32(out, uint32(n)) //nolint:gosec // paths are at most 4096 bytes and an index holds fewer than 2^32 entries
}

func readInt64(b []byte) int64 {
	return int64(binary.BigEndian.Uint64(b)) //nolint:gosec // the manifest stores int64 values in two's complement
}

func boolByte(value bool) byte {
	if value {
		return 1
	}
	return 0
}

func checkPath(path string) error {
	switch {
	case path == "":
		return badManifest("empty path")
	case len(path) > manifestMaxPath:
		return badManifest("path length")
	case strings.IndexByte(path, 0) >= 0:
		return badManifest("NUL in path")
	}
	return nil
}

func encodeManifest(m manifest) ([]byte, error) {
	if m.TrigramSize < 0 {
		return nil, badManifest("trigram size")
	}
	out := make([]byte, 0, manifestHeaderBytes+len(m.Entries)*manifestMinEntry)
	out = append(out, manifestMagic...)
	out = appendInt64(out, m.Start)
	out = appendInt64(out, m.TrigramSize)
	out = appendLength(out, len(m.Entries))
	for i, entry := range m.Entries {
		if err := checkPath(entry.Path); err != nil {
			return nil, err
		}
		if i > 0 && entry.Path <= m.Entries[i-1].Path {
			return nil, badManifest("paths not sorted")
		}
		out = appendLength(out, len(entry.Path))
		out = append(out, entry.Path...)
		out = appendInt64(out, entry.Size)
		out = appendInt64(out, entry.ModNS)
		out = appendInt64(out, entry.CtimeNS)
		out = append(out, boolByte(entry.Indexed))
	}
	return out, nil
}

func decodeFrame(data []byte) (manifestFrame, error) {
	if len(data) < len(manifestMagic) {
		return manifestFrame{}, badManifest("truncated")
	}
	if string(data[:len(manifestMagic)]) != manifestMagic {
		return manifestFrame{}, badManifest("bad magic")
	}
	if len(data) < manifestHeaderBytes {
		return manifestFrame{}, badManifest("truncated")
	}
	rest := data[len(manifestMagic):]
	frame := manifestFrame{head: manifest{Start: readInt64(rest[0:8]), TrigramSize: readInt64(rest[8:16])}}
	if frame.head.TrigramSize < 0 {
		return manifestFrame{}, badManifest("trigram size")
	}
	count := binary.BigEndian.Uint32(rest[16:20])
	frame.body = rest[20:]
	if uint64(count) > uint64(len(frame.body))/manifestMinEntry {
		return manifestFrame{}, badManifest("count mismatch")
	}
	frame.count = int(count)
	return frame, nil
}

func decodeManifest(data []byte) (manifest, error) {
	frame, err := decodeFrame(data)
	if err != nil {
		return manifest{}, err
	}
	m := frame.head
	m.Entries = make([]manifestEntry, 0, frame.count)
	rest := frame.body
	for range frame.count {
		if len(rest) == 0 {
			return manifest{}, badManifest("count mismatch")
		}
		entry, next, err := decodeEntry(rest)
		if err != nil {
			return manifest{}, err
		}
		if len(m.Entries) > 0 && entry.Path <= m.Entries[len(m.Entries)-1].Path {
			return manifest{}, badManifest("paths not sorted")
		}
		m.Entries = append(m.Entries, entry)
		rest = next
	}
	if len(rest) != 0 {
		if startsEntry(rest) {
			return manifest{}, badManifest("count mismatch")
		}
		return manifest{}, badManifest("trailing bytes")
	}
	return m, nil
}

func startsEntry(rest []byte) bool {
	_, _, err := decodeEntry(rest)
	return err == nil
}

func decodeEntry(rest []byte) (manifestEntry, []byte, error) {
	if len(rest) < 4 {
		return manifestEntry{}, nil, badManifest("truncated")
	}
	pathLen := binary.BigEndian.Uint32(rest[:4])
	rest = rest[4:]
	if pathLen == 0 || pathLen > manifestMaxPath {
		return manifestEntry{}, nil, badManifest("path length")
	}
	if uint64(len(rest)) < uint64(pathLen)+manifestTailBytes {
		return manifestEntry{}, nil, badManifest("truncated")
	}
	path := string(rest[:pathLen])
	if err := checkPath(path); err != nil {
		return manifestEntry{}, nil, err
	}
	rest = rest[pathLen:]
	entry := manifestEntry{
		Path:    path,
		Size:    readInt64(rest[0:8]),
		ModNS:   readInt64(rest[8:16]),
		CtimeNS: readInt64(rest[16:24]),
	}
	switch rest[24] {
	case 0:
	case 1:
		entry.Indexed = true
	default:
		return manifestEntry{}, nil, badManifest("bad indexed byte")
	}
	return entry, rest[manifestTailBytes:], nil
}

func (m manifest) lookup(path string) (manifestEntry, bool) {
	i := sort.Search(len(m.Entries), func(i int) bool { return m.Entries[i].Path >= path })
	if i < len(m.Entries) && m.Entries[i].Path == path {
		return m.Entries[i], true
	}
	return manifestEntry{}, false
}

func writeManifest(path string, m manifest) error {
	data, err := encodeManifest(m)
	if err != nil {
		return err
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		return err
	}
	return syncFile(path)
}

func readManifest(path string) (manifest, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return manifest{}, err
	}
	return decodeManifest(data)
}

func syncFile(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	syncErr := file.Sync()
	if closeErr := file.Close(); syncErr == nil {
		return closeErr
	}
	return syncErr
}
