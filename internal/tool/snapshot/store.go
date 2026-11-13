package snapshot

import (
	"bytes"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	maxEntries = 512
	maxBytes   = 64 << 20
)

var (
	ErrUnknownReference = errors.New("snapshot: unknown reference")
	ErrStaleReference   = errors.New("snapshot: stale reference")
	ErrTargetChanged    = errors.New("snapshot: target changed")
	ErrGapChanged       = errors.New("snapshot: gap changed")
)

type Range struct {
	Start int
	End   int
}

type Span struct {
	Start int
	End   int
}

type Splice struct {
	Start    int
	End      int
	Inserted int
}

type Entry struct {
	Ref      string
	Path     string
	Data     []byte
	Shown    []Range
	Blob     string
	Revision uint64
	Epoch    uint64
}

type Edge struct {
	FromRevision uint64
	ToRevision   uint64
	Epoch        uint64
	Before       string
	After        string
	Splices      []Splice
}

type head struct {
	Ref      string
	Digest   string
	Revision uint64
	Epoch    uint64
	Deleted  bool
}

type Store struct {
	mu      sync.RWMutex
	entries map[string]Entry
	heads   map[string]head
	edges   map[string][]Edge
	order   []string
	bytes   int
	boot    [16]byte
	seq     uint64
}

func New() *Store {
	var boot [16]byte
	if _, err := rand.Read(boot[:]); err != nil {
		sum := sha256.Sum256([]byte(strconv.FormatInt(time.Now().UnixNano(), 10)))
		copy(boot[:], sum[:16])
	}
	return &Store{
		entries: make(map[string]Entry),
		heads:   make(map[string]head),
		edges:   make(map[string][]Edge),
		boot:    boot,
	}
}

func (s *Store) Put(path string, data []byte, shown []Range) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.putLocked(path, data, shown)
}

func (s *Store) Capture(path string, data []byte, shown []Range) (Entry, bool) {
	if s == nil {
		return Entry{}, false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	ref := s.putLocked(path, data, shown)
	if ref == "" {
		return Entry{}, false
	}
	entry := s.entries[ref]
	entry.Data = nil
	entry.Shown = slices.Clone(entry.Shown)
	return entry, true
}

func (s *Store) putLocked(path string, data []byte, shown []Range) string {
	ref := s.mintLocked()
	if ref == "" {
		return ""
	}
	body := bytes.Clone(data)
	digest := Digest(body)
	revision, epoch := s.nextCapture(path, digest)
	entry := Entry{
		Ref: ref, Path: path, Data: body, Blob: digest,
		Shown:    mergeRanges(limitShown(shown, lineCount(body))),
		Revision: revision, Epoch: epoch,
	}
	s.insertLocked(entry)
	s.heads[path] = head{Ref: ref, Digest: digest, Revision: revision, Epoch: epoch}
	return ref
}

func (s *Store) nextCapture(path, digest string) (uint64, uint64) {
	current, exists := s.heads[path]
	if !exists {
		return 1, 1
	}
	if current.Deleted || current.Digest != digest {
		return current.Revision + 1, current.Epoch + 1
	}
	return current.Revision, current.Epoch
}

func (s *Store) Get(ref string) (Entry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	entry, ok := s.entries[ref]
	if !ok {
		return Entry{}, ErrUnknownReference
	}
	return cloneEntry(entry), nil
}

func (s *Store) Covers(ref string, first, last int) bool {
	if first < 1 || last < first {
		return false
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	entry, ok := s.entries[ref]
	return ok && rangesCover(entry.Shown, first, last)
}

func (s *Store) Seen(path string, data []byte, first, last int) bool {
	if first < 1 || last < first {
		return false
	}
	digest := Digest(data)
	s.mu.RLock()
	defer s.mu.RUnlock()
	for _, entry := range s.entries {
		if entry.Path == path && entry.Blob == digest && rangesCover(entry.Shown, first, last) {
			return true
		}
	}
	return false
}

func (s *Store) Unchanged(ref, path string, current []byte) bool {
	s.mu.RLock()
	defer s.mu.RUnlock()
	entry, ok := s.entries[ref]
	if !ok || entry.Path != path {
		return false
	}
	latest, ok := s.heads[path]
	return ok && !latest.Deleted && latest.Digest == Digest(current) &&
		latest.Revision == entry.Revision && latest.Epoch == entry.Epoch
}

func (s *Store) Record(path string, before, after []byte, splices []Splice) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.recordLocked(path, before, after, splices)
}

func (s *Store) recordLocked(path string, before, after []byte, splices []Splice) string {
	current, matched := s.matchedHead(path, before)
	if bytes.Equal(before, after) {
		if matched {
			return current.Ref
		}
		return ""
	}
	if !matched || !soundSplices(before, splices) {
		return s.putLocked(path, after, nil)
	}
	return s.appendEdge(path, current, after, splices)
}

func (s *Store) matchedHead(path string, before []byte) (head, bool) {
	current, exists := s.heads[path]
	return current, exists && !current.Deleted && current.Digest == Digest(before)
}

func (s *Store) appendEdge(path string, current head, after []byte, splices []Splice) string {
	ref := s.mintLocked()
	if ref == "" {
		return ""
	}
	digest := Digest(after)
	revision := current.Revision + 1
	s.insertLocked(Entry{
		Ref: ref, Path: path, Data: bytes.Clone(after), Blob: digest,
		Revision: revision, Epoch: current.Epoch,
	})
	ordered := slices.Clone(splices)
	slices.SortStableFunc(ordered, func(a, b Splice) int { return b.Start - a.Start })
	s.edges[path] = append(s.edges[path], Edge{
		FromRevision: current.Revision, ToRevision: revision, Epoch: current.Epoch,
		Before: current.Digest, After: digest, Splices: ordered,
	})
	s.heads[path] = head{Ref: ref, Digest: digest, Revision: revision, Epoch: current.Epoch}
	return ref
}

func (s *Store) Remove(path string, before []byte) {
	s.mu.Lock()
	defer s.mu.Unlock()
	current := s.heads[path]
	if current.Deleted || current.Digest != Digest(before) || current.Epoch == 0 {
		s.heads[path] = head{Deleted: true, Revision: current.Revision + 1, Epoch: current.Epoch + 1}
		return
	}
	current.Deleted = true
	current.Revision++
	s.heads[path] = current
}

func (s *Store) Rename(oldPath, newPath string, before, after []byte, _ []Splice) string {
	s.Remove(oldPath, before)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.breakDest(newPath)
	return s.putLocked(newPath, after, nil)
}

func (s *Store) breakDest(path string) {
	current, ok := s.heads[path]
	if !ok {
		return
	}
	s.heads[path] = head{Deleted: true, Revision: current.Revision + 1, Epoch: current.Epoch + 1}
}

func (s *Store) ResolveSpan(ref, path string, current []byte, target Span) (Span, error) {
	edges, err := s.lineage(ref, path, current)
	if err != nil {
		return Span{}, err
	}
	mapped := target
	for _, edge := range edges {
		for _, splice := range edge.Splices {
			var ok bool
			mapped, ok = MapSpan(mapped, splice)
			if !ok {
				return Span{}, ErrTargetChanged
			}
		}
	}
	return mapped, nil
}

func (s *Store) ResolveGap(ref, path string, current []byte, gap int) (int, error) {
	edges, err := s.lineage(ref, path, current)
	if err != nil {
		return 0, err
	}
	mapped := gap
	for _, edge := range edges {
		for _, splice := range edge.Splices {
			var ok bool
			mapped, ok = MapGap(mapped, splice)
			if !ok {
				return 0, ErrGapChanged
			}
		}
	}
	return mapped, nil
}

func (s *Store) lineage(ref, path string, current []byte) ([]Edge, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	entry, ok := s.entries[ref]
	if !ok || entry.Path != path {
		return nil, ErrUnknownReference
	}
	latest, ok := s.heads[path]
	if !ok || latest.Deleted || latest.Digest != Digest(current) || latest.Epoch != entry.Epoch {
		return nil, ErrStaleReference
	}
	if latest.Revision < entry.Revision {
		return nil, ErrStaleReference
	}
	return walkEdges(s.edges[path], entry.Epoch, entry.Revision, latest.Revision)
}

func walkEdges(all []Edge, epoch, from, to uint64) ([]Edge, error) {
	out := make([]Edge, 0, to-from)
	want := from
	for want < to {
		edge, ok := findEdge(all, epoch, want)
		if !ok {
			return nil, ErrStaleReference
		}
		out = append(out, cloneEdge(edge))
		want++
	}
	return out, nil
}

func findEdge(all []Edge, epoch, from uint64) (Edge, bool) {
	for _, edge := range all {
		if edge.Epoch == epoch && edge.FromRevision == from && edge.ToRevision == from+1 {
			return edge, true
		}
	}
	return Edge{}, false
}

func (s *Store) insertLocked(entry Entry) {
	for len(s.order) >= maxEntries || (s.bytes+len(entry.Data) > maxBytes && len(s.order) > 0) {
		oldest := s.order[0]
		s.order = s.order[1:]
		removed, ok := s.entries[oldest]
		if ok {
			s.bytes -= len(removed.Data)
			delete(s.entries, oldest)
		}
	}
	s.entries[entry.Ref] = entry
	s.order = append(s.order, entry.Ref)
	s.bytes += len(entry.Data)
}

func (s *Store) mintLocked() string {
	if s.seq == ^uint64(0) {
		return ""
	}
	s.seq++
	return "r" + hex.EncodeToString(s.boot[:]) + "." + strconv.FormatUint(s.seq, 36)
}

func (s *Store) noteSeq(ref string) {
	seq := refSequence(ref)
	if seq > s.seq {
		s.seq = seq
	}
}

func ValidRef(ref string) bool {
	rest, ok := strings.CutPrefix(ref, "r")
	if !ok {
		return false
	}
	boot, seq, ok := strings.Cut(rest, ".")
	if !ok || len(boot) != 32 || boot != strings.ToLower(boot) {
		return false
	}
	if _, err := hex.DecodeString(boot); err != nil {
		return false
	}
	value, ok := parseSequence(seq)
	return ok && value != 0
}

func parseSequence(seq string) (uint64, bool) {
	if seq == "" || len(seq) > 13 || seq[0] == '0' {
		return 0, false
	}
	value := uint64(0)
	for _, char := range seq {
		digit, ok := base36(char)
		if !ok || value > (^uint64(0)-digit)/36 {
			return 0, false
		}
		value = value*36 + digit
	}
	return value, true
}

func base36(char rune) (uint64, bool) {
	switch {
	case char >= '0' && char <= '9':
		return uint64(char - '0'), true
	case char >= 'a' && char <= 'z':
		return uint64(char-'a') + 10, true
	default:
		return 0, false
	}
}

func refSequence(ref string) uint64 {
	_, seq, _ := strings.Cut(ref, ".")
	value, _ := parseSequence(seq)
	return value
}

func Digest(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func cloneEntry(entry Entry) Entry {
	entry.Data = bytes.Clone(entry.Data)
	entry.Shown = slices.Clone(entry.Shown)
	return entry
}

func cloneEdge(edge Edge) Edge {
	edge.Splices = slices.Clone(edge.Splices)
	return edge
}

func lineCount(data []byte) int {
	if len(data) == 0 {
		return 0
	}
	count := bytes.Count(data, []byte{'\n'})
	if data[len(data)-1] != '\n' {
		count++
	}
	return count
}

func limitShown(shown []Range, lines int) []Range {
	out := make([]Range, 0, len(shown))
	for _, item := range shown {
		if item.Start >= 1 && item.End >= item.Start && item.End <= lines {
			out = append(out, item)
		}
	}
	return out
}

func mergeRanges(ranges []Range) []Range {
	valid := make([]Range, 0, len(ranges))
	for _, item := range ranges {
		if item.Start > 0 && item.End >= item.Start {
			valid = append(valid, item)
		}
	}
	slices.SortFunc(valid, func(a, b Range) int { return a.Start - b.Start })
	out := make([]Range, 0, len(valid))
	for _, item := range valid {
		if len(out) == 0 || item.Start > out[len(out)-1].End+1 {
			out = append(out, item)
			continue
		}
		out[len(out)-1].End = max(out[len(out)-1].End, item.End)
	}
	return out
}

func rangesCover(ranges []Range, first, last int) bool {
	for _, item := range ranges {
		if item.Start <= first && item.End >= last {
			return true
		}
	}
	return false
}

func soundSplices(before []byte, splices []Splice) bool {
	if len(splices) == 0 {
		return false
	}
	ordered := slices.Clone(splices)
	slices.SortFunc(ordered, func(a, b Splice) int { return a.Start - b.Start })
	prev := 0
	lines := lineCount(before)
	for _, item := range ordered {
		if !spliceFits(item, lines, prev) {
			return false
		}
		prev = nextSpliceBound(item)
	}
	return true
}

func spliceFits(item Splice, lines, prev int) bool {
	if item.Inserted < 0 || item.Start < prev || item.End < item.Start || item.End > lines {
		return false
	}
	return item.Start < lines || item.End == lines
}

func nextSpliceBound(item Splice) int {
	if item.Start == item.End {
		return item.Start + 1
	}
	return item.End
}
