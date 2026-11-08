package journal

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

type reconcileResult struct {
	head     SessionHead
	lastHash string
	rebuild  bool
}

type scanState struct {
	orig       []byte
	rebuilt    []byte
	emits      []emitRec
	lastHash   string
	stopOff    int64
	damagedSeq uint64
	torn       bool
	changed    bool
	rebuild    bool
}

func (s *Store) reconcileAll() (bool, error) {
	entries, err := os.ReadDir(s.sessionDir(""))
	if err != nil {
		return false, fmt.Errorf("journal: read sessions: %w", err)
	}
	needsRebuild := false
	for _, entry := range entries {
		if !entry.IsDir() || !validSessionID(entry.Name()) {
			continue
		}
		sid := entry.Name()
		exists, err := fileExists(s.journalPath(sid))
		if err != nil {
			return false, fmt.Errorf("journal: stat %s: %w", sid, err)
		}
		if !exists {
			continue
		}
		result, err := s.reconcileSession(sid)
		if err != nil {
			return false, err
		}
		s.sessions[sid] = stateOf(result)
		needsRebuild = needsRebuild || result.rebuild
	}
	return needsRebuild, nil
}

func stateOf(result reconcileResult) *sessionState {
	return &sessionState{
		seq:        result.head.HeadSeq,
		hash:       result.lastHash,
		damaged:    result.head.Status == SessionDamaged,
		damagedSeq: result.head.DamagedSeq,
	}
}

func fileExists(path string) (bool, error) {
	_, err := os.Stat(path)
	if err == nil {
		return true, nil
	}
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	return false, err
}

func (s *Store) reconcileSession(sid string) (reconcileResult, error) {
	data, err := os.ReadFile(s.journalPath(sid))
	if err != nil {
		return reconcileResult{}, fmt.Errorf("journal: read %s: %w", sid, err)
	}
	markerSeq, hasMarker, err := readMarker(s.sessionDir(sid))
	if err != nil {
		return reconcileResult{}, err
	}
	mirror := s.idx.events[sid]
	if hasMarker {
		mirror = nil
	}
	scan, err := s.scanJournal(sid, data, mirror)
	if err != nil {
		return reconcileResult{}, err
	}
	if hasMarker {
		scan.damagedSeq = markerSeq
	}
	head := SessionHead{ID: sid, HeadSeq: uint64(len(scan.emits)), Status: SessionOK}
	if scan.damagedSeq > 0 {
		head.Status = SessionDamaged
		head.DamagedSeq = scan.damagedSeq
	}
	rebuild, err := s.finishReconcile(sid, &scan, head, hasMarker)
	if err != nil {
		return reconcileResult{}, err
	}
	return reconcileResult{head: head, lastHash: scan.lastHash, rebuild: rebuild}, nil
}

func (s *Store) scanJournal(sid string, data []byte, si *sessIndex) (scanState, error) {
	scan := scanState{
		orig:     data,
		rebuilt:  make([]byte, 0, len(data)),
		emits:    make([]emitRec, 0),
		lastHash: genesisPrev,
		stopOff:  -1,
	}
	lines := splitLines(data)
	endsNL := len(data) > 0 && data[len(data)-1] == '\n'
	for i := range lines {
		line := linePosition{
			record:   lines[i],
			seq:      uint64(i + 1),
			last:     i == len(lines)-1,
			complete: i < len(lines)-1 || endsNL,
		}
		stop, err := s.scanLine(sid, &scan, line, si)
		if err != nil {
			return scanState{}, err
		}
		if stop {
			break
		}
	}
	if scan.damagedSeq == 0 {
		if err := s.restoreTail(sid, &scan, si); err != nil {
			return scanState{}, err
		}
	}
	return scan, nil
}

type linePosition struct {
	record   record
	seq      uint64
	last     bool
	complete bool
}

func (s *Store) scanLine(sid string, scan *scanState, line linePosition, si *sessIndex) (bool, error) {
	raw := scan.orig[line.record.lineOff:line.record.lineEnd]
	verified := verifyRecord(raw, line.seq, scan.lastHash)
	if verified.kind == lineOK && line.complete {
		appendScannedRecord(scan, raw, verified)
		return false, nil
	}
	if tornTail(line.last, line.complete, verified.kind) {
		scan.stopOff = line.record.lineOff
		scan.torn = true
		scan.changed = true
		return true, nil
	}
	scan.stopOff = line.record.lineOff
	if err := s.repairDamagedLine(sid, scan, line.seq, si); err != nil {
		return false, err
	}
	return true, nil
}

func tornTail(last, complete bool, kind lineKind) bool {
	if !last {
		return false
	}
	return !complete || kind == lineParse || kind == lineCRC || kind == lineHash
}

func appendScannedRecord(scan *scanState, raw []byte, verified record) {
	scan.rebuilt = append(scan.rebuilt, raw...)
	scan.rebuilt = append(scan.rebuilt, '\n')
	scan.lastHash = verified.hash
	scan.emits = append(scan.emits, emitRec{seq: verified.seq, hash: verified.hash, ev: verified.ev})
}

func (s *Store) repairDamagedLine(sid string, scan *scanState, start uint64, si *sessIndex) error {
	if si == nil || si.head < start {
		scan.damagedSeq = start
		return nil
	}
	staged := make([]emitRec, 0, si.head-start+1)
	prevHash := scan.lastHash
	for seq := start; seq <= si.head; seq++ {
		rec, ok, err := s.fetchVerified(sid, seq, prevHash)
		if err != nil {
			return err
		}
		if !ok {
			scan.damagedSeq = start
			return nil
		}
		staged = append(staged, rec)
		prevHash = rec.hash
	}
	prevHash = scan.lastHash
	for _, rec := range staged {
		scan.rebuilt = encodeLine(scan.rebuilt, rec.seq, prevHash, rec.hash, rec.ev)
		scan.emits = append(scan.emits, rec)
		scan.lastHash = rec.hash
		prevHash = rec.hash
	}
	scan.changed = true
	return nil
}

func (s *Store) restoreTail(sid string, scan *scanState, si *sessIndex) error {
	if si == nil || si.head <= uint64(len(scan.emits)) {
		return nil
	}
	start := uint64(len(scan.emits)) + 1
	staged := make([]emitRec, 0, si.head-start+1)
	prevHash := scan.lastHash
	for seq := start; seq <= si.head; seq++ {
		rec, ok, err := s.fetchVerified(sid, seq, prevHash)
		if err != nil {
			return err
		}
		if !ok {
			scan.rebuild = true
			return nil
		}
		staged = append(staged, rec)
		prevHash = rec.hash
	}
	prevHash = scan.lastHash
	for _, rec := range staged {
		scan.rebuilt = encodeLine(scan.rebuilt, rec.seq, prevHash, rec.hash, rec.ev)
		scan.emits = append(scan.emits, rec)
		scan.lastHash = rec.hash
		prevHash = rec.hash
	}
	scan.changed = true
	return nil
}

func (s *Store) fetchVerified(sid string, seq uint64, prevHash string) (emitRec, bool, error) {
	result, err := fetchRow(s.db, sid, seq)
	if err != nil || !result.ok {
		return emitRec{}, false, err
	}
	if hexOfSum(hashRecord(prevHash, seq, result.ev)) != result.hash || validEventJSON(result.ev) != nil {
		return emitRec{}, false, nil
	}
	return emitRec{seq: seq, hash: result.hash, ev: result.ev}, true, nil
}

func (s *Store) finishReconcile(sid string, scan *scanState, head SessionHead, hasMarker bool) (bool, error) {
	if err := s.persistRepair(sid, scan, hasMarker); err != nil {
		return false, err
	}
	if scan.damagedSeq > 0 {
		return scan.rebuild, upsertSession(s.db, sid, string(head.Status), head.HeadSeq)
	}
	if !s.shared && (scan.rebuild || mirrorHashMismatch(s.idx.events[sid], scan.emits)) {
		return true, nil
	}
	if mirrorMatches(s.idx.events[sid], scan.emits) {
		return false, upsertSession(s.db, sid, string(head.Status), head.HeadSeq)
	}
	return false, replaceSession(s.db, sid, scan.emits, head)
}

func mirrorMatches(si *sessIndex, emits []emitRec) bool {
	if si == nil || len(si.hashes) != len(emits) {
		return false
	}
	for _, rec := range emits {
		if si.hashes[rec.seq] != rec.hash {
			return false
		}
	}
	return true
}

func (s *Store) persistRepair(sid string, scan *scanState, hasMarker bool) error {
	if scan.stopOff >= 0 {
		if err := s.quarantineSuffix(sid, scan); err != nil {
			return err
		}
	}
	if scan.damagedSeq > 0 && !hasMarker {
		if err := writeMarker(s.sessionDir(sid), scan.damagedSeq); err != nil {
			return err
		}
	}
	return persistRebuiltJournal(s.sessionDir(sid), scan)
}

func (s *Store) quarantineSuffix(sid string, scan *scanState) error {
	name := fmt.Sprintf("%s-%d.bin", sid, scan.stopOff)
	if err := writeQuarantine(s.quarantineDir(), name, scan.orig[scan.stopOff:]); err != nil {
		return fmt.Errorf("journal: quarantine %s: %w", sid, err)
	}
	if message := quarantineMessage(sid, scan, name); message != "" {
		slog.Warn(strings.ReplaceAll(strings.ReplaceAll(message, "\r", `\r`), "\n", `\n`))
	}
	return nil
}

func quarantineMessage(sid string, scan *scanState, name string) string {
	if scan.torn {
		return fmt.Sprintf("journal: %s: torn tail of %d bytes quarantined", sid, len(scan.orig)-int(scan.stopOff))
	}
	if scan.damagedSeq > 0 {
		return fmt.Sprintf("journal: %s: damaged at seq %d; quarantined at quarantine/%s", sid, scan.damagedSeq, name)
	}
	return ""
}

func mirrorHashMismatch(si *sessIndex, emits []emitRec) bool {
	if si == nil {
		return false
	}
	for _, rec := range emits {
		hash, ok := si.hashes[rec.seq]
		if ok && hash != rec.hash {
			return true
		}
	}
	return false
}

func persistRebuiltJournal(dir string, scan *scanState) error {
	if !scan.changed && scan.damagedSeq == 0 || bytes.Equal(scan.orig, scan.rebuilt) {
		return nil
	}
	if scan.torn {
		return persistTornJournal(dir, scan)
	}
	return rewriteJournal(dir, scan.rebuilt)
}

func persistTornJournal(dir string, scan *scanState) error {
	path := filepath.Join(dir, "journal.jsonl")
	if err := truncateJournal(path, int(scan.stopOff)); err != nil {
		return err
	}
	if len(scan.rebuilt) == int(scan.stopOff) {
		return nil
	}
	if err := appendJournalTail(path, scan.rebuilt[scan.stopOff:]); err != nil {
		return err
	}
	return syncDir(dir)
}

func appendJournalTail(path string, data []byte) (retErr error) {
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, file.Close()) }()
	n, err := file.Write(data)
	if err != nil {
		return err
	}
	if n != len(data) {
		return ioShortWrite(path, n, len(data))
	}
	return file.Sync()
}

func readMarker(dir string) (uint64, bool, error) {
	data, err := os.ReadFile(filepath.Join(dir, "damaged"))
	if errors.Is(err, fs.ErrNotExist) {
		return 0, false, nil
	}
	if err != nil {
		return 0, false, err
	}
	seq, err := strconv.ParseUint(strings.TrimSpace(string(data)), 10, 64)
	if err != nil {
		return 0, false, fmt.Errorf("journal: bad damaged marker: %w", err)
	}
	return seq, true, nil
}

func writeMarker(dir string, seq uint64) error {
	return writeFileSync(filepath.Join(dir, "damaged"), []byte(strconv.FormatUint(seq, 10)+"\n"))
}

func writeFileSync(path string, data []byte) (err error) {
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, file.Close()) }()
	n, err := file.Write(data)
	if err != nil {
		return err
	}
	if n != len(data) {
		return ioShortWrite(path, n, len(data))
	}
	return file.Sync()
}

func ioShortWrite(path string, written, total int) error {
	return fmt.Errorf("journal: write %s: short write %d of %d", path, written, total)
}

func syncDir(dir string) (err error) {
	file, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, file.Close()) }()
	return file.Sync()
}

func rewriteJournal(dir string, data []byte) error {
	tmp := filepath.Join(dir, "journal.jsonl.tmp")
	if err := writeFileSync(tmp, data); err != nil {
		return err
	}
	if err := os.Rename(tmp, filepath.Join(dir, "journal.jsonl")); err != nil {
		return err
	}
	return syncDir(dir)
}

func truncateJournal(path string, size int) (retErr error) {
	file, err := os.OpenFile(path, os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, file.Close()) }()
	if err := file.Truncate(int64(size)); err != nil {
		return err
	}
	if err := file.Sync(); err != nil {
		return err
	}
	return syncDir(filepath.Dir(path))
}

func writeQuarantine(dir, name string, data []byte) error {
	if err := writeFileSync(filepath.Join(dir, name), data); err != nil {
		return err
	}
	return syncDir(dir)
}
