package snapshot

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"strconv"
	"strings"
)

func (e Entry) Meta() jsontext.Value {
	out := []byte(`{"ref":`)
	out = AppendJSONString(out, e.Ref)
	out = append(out, `,"path":`...)
	out = AppendJSONString(out, e.Path)
	out = append(out, `,"blob":`...)
	out = AppendJSONString(out, e.Blob)
	out = append(out, `,"shown":[`...)
	for i, item := range e.Shown {
		if i > 0 {
			out = append(out, ',')
		}
		out = append(out, '[')
		out = strconv.AppendInt(out, int64(item.Start), 10)
		out = append(out, ',')
		out = strconv.AppendInt(out, int64(item.End), 10)
		out = append(out, ']')
	}
	out = append(out, `],"revision":`...)
	out = strconv.AppendUint(out, e.Revision, 10)
	out = append(out, `,"epoch":`...)
	out = strconv.AppendUint(out, e.Epoch, 10)
	out = append(out, '}')
	return jsontext.Value(out)
}

func (s *Store) Restore(meta jsontext.Value, data []byte) error {
	entry, err := decodeMeta(meta)
	if err != nil {
		return err
	}
	if err := checkRestore(entry, data); err != nil {
		return err
	}
	entry.Data = bytes.Clone(data)
	entry.Shown = mergeRanges(entry.Shown)
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.restoreLocked(entry)
}

func checkRestore(entry Entry, data []byte) error {
	if !ValidRef(entry.Ref) {
		return fmt.Errorf("snapshot: invalid reference %q", entry.Ref)
	}
	if entry.Path == "" {
		return errors.New("snapshot: metadata path is empty")
	}
	if entry.Blob != Digest(data) {
		return errors.New("snapshot: metadata blob does not match data")
	}
	return validateShown(entry.Shown, lineCount(data))
}

func (s *Store) restoreLocked(entry Entry) error {
	if have, ok := s.entries[entry.Ref]; ok {
		if !sameSnapshot(have, entry) {
			return fmt.Errorf("snapshot: reference %s conflicts with stored snapshot", entry.Ref)
		}
		return nil
	}
	if err := s.adoptHead(entry); err != nil {
		return err
	}
	s.noteSeq(entry.Ref)
	s.insertLocked(entry)
	return nil
}

func (s *Store) adoptHead(entry Entry) error {
	current, exists := s.heads[entry.Path]
	if !exists {
		s.heads[entry.Path] = capturedHead(entry)
		return nil
	}
	if sameGeneration(current, entry) {
		if current.Deleted || current.Digest != entry.Blob {
			return errors.New("snapshot: metadata conflicts with head")
		}
		return nil
	}
	if newerHead(current, entry) {
		return nil
	}
	s.heads[entry.Path] = capturedHead(entry)
	return nil
}

func capturedHead(entry Entry) head {
	return head{Ref: entry.Ref, Digest: entry.Blob, Revision: entry.Revision, Epoch: entry.Epoch}
}

func sameGeneration(current head, entry Entry) bool {
	return current.Epoch == entry.Epoch && current.Revision == entry.Revision
}

func newerHead(current head, entry Entry) bool {
	return current.Epoch > entry.Epoch || (current.Epoch == entry.Epoch && current.Revision > entry.Revision)
}

func sameSnapshot(have, entry Entry) bool {
	return have.Path == entry.Path && have.Blob == entry.Blob && have.Revision == entry.Revision &&
		have.Epoch == entry.Epoch && rangesEqual(have.Shown, entry.Shown)
}

func validateShown(shown []Range, lines int) error {
	for _, item := range shown {
		if item.Start < 1 || item.End < item.Start || item.End > lines {
			return errors.New("snapshot: shown range is outside the captured file")
		}
	}
	return nil
}

func decodeMeta(meta jsontext.Value) (Entry, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(meta))
	start, err := decoder.ReadToken()
	if err != nil {
		return Entry{}, fmt.Errorf("snapshot: decode metadata: %w", err)
	}
	if start.Kind() != '{' {
		return Entry{}, errors.New("snapshot: metadata must be an object")
	}
	var entry Entry
	seen := map[string]bool{}
	for decoder.PeekKind() != '}' {
		if err := readField(decoder, &entry, seen); err != nil {
			return Entry{}, err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return Entry{}, fmt.Errorf("snapshot: decode metadata end: %w", err)
	}
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return Entry{}, errors.New("snapshot: metadata has trailing data")
	}
	if err := requireMeta(entry, seen); err != nil {
		return Entry{}, err
	}
	return entry, nil
}

func readField(decoder *jsontext.Decoder, entry *Entry, seen map[string]bool) error {
	name, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("snapshot: decode metadata key: %w", err)
	}
	key := name.String()
	value, err := decoder.ReadValue()
	if err != nil {
		return fmt.Errorf("snapshot: decode metadata value: %w", err)
	}
	return applyField(entry, seen, key, value)
}

func applyField(entry *Entry, seen map[string]bool, key string, value jsontext.Value) error {
	if seen[key] {
		return fmt.Errorf("snapshot: duplicate metadata key %q", key)
	}
	seen[key] = true
	if err := assignField(entry, key, value); err != nil {
		return fmt.Errorf("snapshot: metadata %s: %w", key, err)
	}
	return nil
}

func assignField(entry *Entry, key string, value jsontext.Value) error {
	var err error
	switch key {
	case "ref":
		entry.Ref, err = metaString(value)
	case "path":
		entry.Path, err = metaString(value)
	case "blob":
		entry.Blob, err = metaString(value)
	case "shown":
		entry.Shown, err = metaRanges(value)
	case "revision":
		entry.Revision, err = metaUint(value)
	case "epoch":
		entry.Epoch, err = metaUint(value)
	default:
		err = fmt.Errorf("unknown key %q", key)
	}
	return err
}

func requireMeta(entry Entry, seen map[string]bool) error {
	for _, key := range []string{"ref", "path", "blob", "shown", "revision", "epoch"} {
		if !seen[key] {
			return fmt.Errorf("snapshot: metadata missing %s", key)
		}
	}
	if entry.Revision == 0 || entry.Epoch == 0 {
		return errors.New("snapshot: metadata revision and epoch must be positive")
	}
	return nil
}

func metaString(value jsontext.Value) (string, error) {
	text, kind, err := soleValue(value)
	if err != nil {
		return "", err
	}
	if kind != '"' {
		return "", errors.New("must be a string")
	}
	return text, nil
}

func metaUint(value jsontext.Value) (uint64, error) {
	text, kind, err := soleValue(value)
	if err != nil {
		return 0, err
	}
	if !numberKind(kind) || strings.ContainsAny(text, ".eE+") {
		return 0, errors.New("must be an unsigned integer")
	}
	return strconv.ParseUint(text, 10, 64)
}

func soleValue(value jsontext.Value) (string, jsontext.Kind, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", 0, err
	}
	text, kind := token.String(), token.Kind()
	if err := rejectTail(decoder); err != nil {
		return "", 0, err
	}
	return text, kind, nil
}

func metaRanges(value jsontext.Value) ([]Range, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	start, err := decoder.ReadToken()
	if err != nil || start.Kind() != '[' {
		return nil, errors.New("shown must be an array")
	}
	var out []Range
	for decoder.PeekKind() != ']' {
		item, err := decoder.ReadValue()
		if err != nil {
			return nil, err
		}
		pair, err := metaPair(item)
		if err != nil {
			return nil, err
		}
		out = append(out, pair)
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	return out, rejectTail(decoder)
}

func metaPair(value jsontext.Value) (Range, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	start, err := decoder.ReadToken()
	if err != nil || start.Kind() != '[' {
		return Range{}, errors.New("shown range must be an array")
	}
	first, err := readInt(decoder)
	if err != nil {
		return Range{}, err
	}
	second, err := readInt(decoder)
	if err != nil {
		return Range{}, err
	}
	end, err := decoder.ReadToken()
	if err != nil || end.Kind() != ']' {
		return Range{}, errors.New("shown range must have two entries")
	}
	if err := rejectTail(decoder); err != nil {
		return Range{}, err
	}
	return Range{Start: first, End: second}, nil
}

func readInt(decoder *jsontext.Decoder) (int, error) {
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, err
	}
	if !numberKind(token.Kind()) || strings.ContainsAny(token.String(), ".eE+") {
		return 0, errors.New("shown range must be an integer")
	}
	return strconv.Atoi(token.String())
}

func numberKind(kind jsontext.Kind) bool {
	return kind == '0' || (kind >= '1' && kind <= '9')
}

func rejectTail(decoder *jsontext.Decoder) error {
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return errors.New("trailing data")
	}
	return nil
}
