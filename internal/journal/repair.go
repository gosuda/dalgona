package journal

import (
	"bufio"
	"bytes"
	"errors"
	"fmt"
	"io"
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
	lines      []record
	endsNL     bool
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
		rebuilt, err := s.reconcileOne(entry.Name())
		if err != nil {
			return false, err
		}
		needsRebuild = needsRebuild || rebuilt
	}
	return needsRebuild, nil
}

func (s *Store) reconcileOne(sid string) (bool, error) {
	exists, err := fileExists(s.journalPath(sid))
	if err != nil {
		return false, fmt.Errorf("journal: stat %s: %w", sid, err)
	}
	if !exists {
		return false, nil
	}
	si, err := loadSessionEvents(s.db, sid)
	if err != nil {
		return false, fmt.Errorf("journal: load mirror index %s: %w", sid, err)
	}
	reconciled, clean, err := s.reconcileClean(sid, si)
	if err != nil {
		return false, err
	}
	if clean {
		s.sessions[sid] = stateOf(reconciled)
		return false, nil
	}
	result, err := s.reconcileSession(sid, si)
	if err != nil {
		return false, err
	}
	s.sessions[sid] = stateOf(result)
	return result.rebuild, nil
}

func (s *Store) reconcileClean(sid string, si *sessIndex) (reconcileResult, bool, error) {
	verified, clean, err := s.verifyCleanJournal(sid, si)
	if err != nil || !clean {
		return reconcileResult{}, false, err
	}
	if err := upsertSession(s.db, sid, string(SessionOK), verified.head.HeadSeq); err != nil {
		return reconcileResult{}, false, err
	}
	return verified, true, nil
}

func (s *Store) verifyCleanJournal(sid string, si *sessIndex) (reconcileResult, bool, error) {
	empty := reconcileResult{}
	file, skip, err := s.openCleanJournal(sid)
	if err != nil || skip {
		return empty, false, err
	}
	defer func() { _ = file.Close() }()
	return scanCleanJournal(sid, file, si)
}

func (s *Store) openCleanJournal(sid string) (*os.File, bool, error) {
	_, marked, err := readMarker(s.sessionDir(sid))
	if err != nil {
		return nil, false, err
	}
	if marked {
		return nil, true, nil
	}
	file, err := os.Open(s.journalPath(sid))
	if err != nil {
		return nil, false, err
	}
	return file, false, nil
}

func scanCleanJournal(sid string, file *os.File, si *sessIndex) (reconcileResult, bool, error) {
	empty := reconcileResult{}
	reader := bufio.NewReaderSize(file, 64*1024)
	var scratch []byte
	prev := genesisPrev
	var seq uint64
	for {
		line, hadNL, readErr := readJournalLine(reader, &scratch)
		if line != nil {
			if !hadNL {
				return empty, false, nil
			}
			seq++
			next, ok := checkCleanLine(si, line, seq, prev)
			if !ok {
				return empty, false, nil
			}
			prev = next
		}
		if readErr == nil {
			continue
		}
		if errors.Is(readErr, io.EOF) {
			break
		}
		return empty, false, readErr
	}
	return cleanResult(sid, si, seq, prev)
}

func cleanResult(sid string, si *sessIndex, seq uint64, prev string) (reconcileResult, bool, error) {
	if si != nil && si.head != seq {
		return reconcileResult{}, false, nil
	}
	return reconcileResult{
		head:     SessionHead{ID: sid, HeadSeq: seq, Status: SessionOK},
		lastHash: prev,
	}, true, nil
}

func checkCleanLine(si *sessIndex, line []byte, seq uint64, prev string) (string, bool) {
	verified := verifyRecord(line, seq, prev)
	if verified.kind != lineOK {
		return "", false
	}
	if si == nil || si.head < seq || si.hashes[seq] != verified.hash {
		return "", false
	}
	return verified.hash, true
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

func (s *Store) reconcileSession(sid string, mirror *sessIndex) (reconcileResult, error) {
	data, err := os.ReadFile(s.journalPath(sid)) //nolint:gosec // session journal path is validated
	if err != nil {
		return reconcileResult{}, fmt.Errorf("journal: read %s: %w", sid, err)
	}
	markerSeq, hasMarker, err := readMarker(s.sessionDir(sid))
	if err != nil {
		return reconcileResult{}, err
	}
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
	plan := reconcilePlan{scan: &scan, head: head, hasMarker: hasMarker, mirror: mirror}
	rebuild, err := s.finishReconcile(sid, plan)
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
	scan.lines = lines
	scan.endsNL = endsNL
	for i := range lines {
		stop, err := s.scanLine(sid, &scan, i, uint64(i)+1, si)
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

func (s *Store) scanLine(sid string, scan *scanState, idx int, seq uint64, si *sessIndex) (bool, error) {
	line := linePosition{
		record:   scan.lines[idx],
		seq:      seq,
		last:     idx == len(scan.lines)-1,
		complete: idx < len(scan.lines)-1 || scan.endsNL,
	}
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
	if err := s.repairDamagedLine(sid, scan, seq, si); err != nil {
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
	appendValidTail(scan, si.head)
	return nil
}

func appendValidTail(scan *scanState, resume uint64) {
	for idx := range scan.lines {
		seq := uint64(idx) + 1
		if seq <= resume {
			continue
		}
		raw := scan.orig[scan.lines[idx].lineOff:scan.lines[idx].lineEnd]
		verified := verifyRecord(raw, seq, scan.lastHash)
		if verified.kind == lineOK && (idx < len(scan.lines)-1 || scan.endsNL) {
			appendScannedRecord(scan, raw, verified)
			continue
		}
		scan.damagedSeq = seq
		return
	}
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
	if hexOfSum(hashRecord(prevHash, seq, result.ev)) != result.hash || !isEventJSON(result.ev) {
		return emitRec{}, false, nil
	}
	return emitRec{seq: seq, hash: result.hash, ev: result.ev}, true, nil
}

func isEventJSON(ev []byte) bool {
	return validEventJSON(ev) == nil
}

type reconcilePlan struct {
	scan      *scanState
	head      SessionHead
	hasMarker bool
	mirror    *sessIndex
}

func (s *Store) finishReconcile(sid string, plan reconcilePlan) (bool, error) {
	if err := s.persistRepair(sid, plan.scan, plan.hasMarker); err != nil {
		return false, err
	}
	if plan.scan.damagedSeq > 0 {
		if mirrorMatches(plan.mirror, plan.scan.emits) {
			return plan.scan.rebuild, upsertSession(s.db, sid, string(plan.head.Status), plan.head.HeadSeq)
		}
		return plan.scan.rebuild, replaceSession(s.db, sid, plan.scan.emits, plan.head)
	}
	if !s.shared && (plan.scan.rebuild || mirrorHashMismatch(plan.mirror, plan.scan.emits)) {
		return true, nil
	}
	if mirrorMatches(plan.mirror, plan.scan.emits) {
		return false, upsertSession(s.db, sid, string(plan.head.Status), plan.head.HeadSeq)
	}
	return false, replaceSession(s.db, sid, plan.scan.emits, plan.head)
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
	dir := s.quarantineDir()
	base := fmt.Sprintf("%s-%d", sid, scan.stopOff)
	data := scan.orig[scan.stopOff:]
	for attempt := range 100 {
		candidate := base + ".bin"
		if attempt > 0 {
			candidate = fmt.Sprintf("%s-%d.bin", base, quarantineSeq.Add(1))
		}
		writeErr := writeQuarantine(dir, candidate, data)
		if writeErr == nil {
			if message := quarantineMessage(sid, scan, candidate); message != "" {
				slog.Warn(strings.ReplaceAll(strings.ReplaceAll(message, "\r", `\r`), "\n", `\n`))
			}
			return nil
		}
		if !errors.Is(writeErr, os.ErrExist) {
			return fmt.Errorf("journal: quarantine %s: %w", sid, writeErr)
		}
	}
	return fmt.Errorf("journal: quarantine %s: names exhausted", sid)
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
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0o600) //nolint:gosec // journal path is validated
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
	data, err := os.ReadFile(filepath.Join(dir, "damaged")) //nolint:gosec // damaged marker path is validated
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
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600) //nolint:gosec // file path is validated
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

func rewriteJournal(dir string, data []byte) error {
	tmp := filepath.Join(dir, "journal.jsonl.tmp")
	if err := writeFileSync(tmp, data); err != nil {
		return err
	}
	if err := os.Rename(tmp, filepath.Join(dir, "journal.jsonl")); err != nil { //nolint:gosec // journal paths are validated
		return err
	}
	return syncDir(dir)
}

func truncateJournal(path string, size int) (retErr error) {
	file, err := os.OpenFile(path, os.O_WRONLY, 0o600) //nolint:gosec // journal path is validated
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

func writeQuarantine(dir, name string, data []byte) (retErr error) {
	path := filepath.Join(dir, name)
	file, openErr := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600) //nolint:gosec // quarantine path is validated
	if openErr != nil {
		return openErr
	}
	defer func() {
		retErr = errors.Join(retErr, file.Close())
	}()
	written, writeErr := file.Write(data)
	if writeErr != nil {
		return writeErr
	}
	if written != len(data) {
		return ioShortWrite(path, written, len(data))
	}
	if syncErr := file.Sync(); syncErr != nil {
		return syncErr
	}
	if dirErr := syncDir(dir); dirErr != nil {
		return dirErr
	}
	return nil
}
