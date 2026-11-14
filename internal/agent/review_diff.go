package agent

import (
	"bytes"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/textdiff"
)

var (
	reviewConcurrency = regexp.MustCompile(`\bgo\s+\w|\bchan\b|\bsync\.|\batomic\.|\bselect\s*\{|Mutex|WaitGroup|\basync\b|\bawait\b|Promise|\bWorker\b|\bspawn\b|Thread|threading|asyncio|tokio|\bArc<|setTimeout|setInterval`)
	hunkHeader        = regexp.MustCompile(`^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@`)
)

type reviewFile struct {
	path, change, skip string
	pre, post          []byte
}

type hunk struct {
	oldStart, newStart int
	lines              []string
}

type fileDiff struct {
	path, change, kind, header string
	hunks                      []hunk
	added, removed             int
	concurrent, binary         bool
}

type reviewUnit struct {
	id, file int
	hunks    []hunk
}

type placed struct {
	unit, file int
	hunks      []hunk
	piece      bool
}

type partFile struct {
	index     int
	file      *fileDiff
	hunks     []hunk
	truncated bool
}

type judgePart struct {
	Index int `json:"index"`
	Of    int `json:"of"`
}

type judgeFile struct {
	Path      string `json:"path"`
	Kind      string `json:"kind"`
	Change    string `json:"change"`
	Added     int    `json:"added"`
	Removed   int    `json:"removed"`
	Diff      string `json:"diff"`
	Truncated bool   `json:"truncated"`
}

type judgeState struct {
	Task         string      `json:"task,omitempty"`
	Tool         string      `json:"tool,omitempty"`
	Files        []judgeFile `json:"files"`
	Part         judgePart   `json:"part"`
	OmittedHunks int         `json:"omitted_hunks"`
}

type judgeInput struct {
	state     jsontext.Value
	questions map[string]decide.Question
	truncated bool
}

type unitInput struct {
	parts     []judgeInput
	paths     []string
	truncated bool
	added     int
	err       error
}

func classifyReviewFile(path, change string, pre, post []byte) reviewFile {
	file := reviewFile{path: path, change: change, pre: pre, post: post}
	switch {
	case len(pre) > textdiff.Limit || len(post) > textdiff.Limit:
		file.skip = "large"
	case !reviewText(pre) || !reviewText(post):
		file.skip = "binary"
	}
	return file
}

func reviewText(data []byte) bool {
	return !bytes.Contains(data, []byte{0}) && utf8.Valid(data)
}

func reviewKind(path string) string {
	switch filepath.Ext(path) {
	case ".css", ".scss", ".sass", ".less", ".svelte", ".vue", ".jsx", ".tsx", ".html", ".htm":
		return "interface"
	case ".toml", ".yaml", ".yml", ".json", ".ini", ".env", ".lock":
		return "config"
	case ".md", ".mdx", ".txt", ".rst":
		return "text"
	}
	return "code"
}

func diffFiles(files []reviewFile, contextLines int) ([]fileDiff, error) {
	out := make([]fileDiff, 0, len(files))
	for _, file := range files {
		if file.skip == "large" {
			continue
		}
		fd, ok, err := diffOne(file, contextLines)
		if err != nil {
			return nil, err
		}
		if ok {
			out = append(out, fd)
		}
	}
	return out, nil
}

func diffOne(file reviewFile, contextLines int) (fileDiff, bool, error) {
	fd := fileDiff{path: file.path, change: file.change, kind: reviewKind(file.path)}
	if file.skip == "binary" {
		fd.binary, fd.header = true, "[binary file]\n"
		return fd, true, nil
	}
	oldLabel, newLabel := "a/"+file.path, "b/"+file.path
	if file.change == "add" {
		oldLabel = "/dev/null"
	}
	if file.change == "delete" {
		newLabel = "/dev/null"
	}
	diff, stats, err := textdiff.Unified(oldLabel, newLabel, file.pre, file.post, contextLines)
	if err != nil {
		return fileDiff{}, false, err
	}
	header, hunks, err := splitHunks(diff)
	if err != nil {
		return fileDiff{}, false, err
	}
	if len(hunks) == 0 {
		return fileDiff{}, false, nil
	}
	fd.header, fd.hunks, fd.added, fd.removed = header, hunks, stats.Added, stats.Removed
	fd.concurrent = hasConcurrentLine(hunks)
	return fd, true, nil
}

func hasConcurrentLine(hunks []hunk) bool {
	for _, h := range hunks {
		for _, line := range h.lines {
			if strings.HasPrefix(line, "+") && reviewConcurrency.MatchString(line[1:]) {
				return true
			}
		}
	}
	return false
}

func splitHunks(diff string) (string, []hunk, error) {
	var header strings.Builder
	var hunks []hunk
	for _, line := range strings.SplitAfter(diff, "\n") {
		if line == "" {
			continue
		}
		if strings.HasPrefix(line, "@@ ") {
			m := hunkHeader.FindStringSubmatch(line)
			if m == nil {
				return "", nil, fmt.Errorf("review: bad hunk header %q", strings.TrimSpace(line))
			}
			oldStart, _ := strconv.Atoi(m[1])
			newStart, _ := strconv.Atoi(m[2])
			hunks = append(hunks, hunk{oldStart: oldStart, newStart: newStart})
			continue
		}
		if len(hunks) == 0 {
			header.WriteString(line)
			continue
		}
		last := &hunks[len(hunks)-1]
		last.lines = append(last.lines, line)
	}
	return header.String(), hunks, nil
}

func (h hunk) text() string {
	oldCount, newCount := sideCounts(h.lines)
	var b strings.Builder
	b.WriteString("@@ -")
	b.WriteString(hunkRange(h.oldStart, oldCount))
	b.WriteString(" +")
	b.WriteString(hunkRange(h.newStart, newCount))
	b.WriteString(" @@\n")
	for _, line := range h.lines {
		b.WriteString(line)
	}
	return b.String()
}

func hunkRange(start, count int) string {
	switch {
	case count == 0 && start == 1:
		return "0,0"
	case count > 1:
		return strconv.Itoa(start) + "," + strconv.Itoa(count)
	}
	return strconv.Itoa(start)
}

func sideCounts(lines []string) (oldCount, newCount int) {
	for _, line := range lines {
		switch {
		case strings.HasPrefix(line, " "):
			oldCount++
			newCount++
		case strings.HasPrefix(line, "-"):
			oldCount++
		case strings.HasPrefix(line, "+"):
			newCount++
		}
	}
	return oldCount, newCount
}

func splitHunk(h hunk, fits func(hunk) bool) ([]hunk, bool) {
	var pieces []hunk
	rest := h
	for len(rest.lines) > 0 {
		k := largestFitting(rest, fits)
		if k == 0 {
			return pieces, false
		}
		cut := splitCut(rest.lines, k)
		piece := hunk{oldStart: rest.oldStart, newStart: rest.newStart, lines: rest.lines[:cut]}
		pieces = append(pieces, piece)
		oldCount, newCount := sideCounts(piece.lines)
		rest = hunk{oldStart: rest.oldStart + oldCount, newStart: rest.newStart + newCount, lines: rest.lines[cut:]}
	}
	return pieces, true
}

func largestFitting(h hunk, fits func(hunk) bool) int {
	lo, hi := 0, len(h.lines)
	for lo < hi {
		mid := (lo + hi + 1) / 2
		if fits(hunk{oldStart: h.oldStart, newStart: h.newStart, lines: h.lines[:mid]}) {
			lo = mid
		} else {
			hi = mid - 1
		}
	}
	return lo
}

func splitCut(lines []string, k int) int {
	cut := blankCut(lines, k)
	for cut < len(lines) && strings.HasPrefix(lines[cut], `\`) {
		cut++
	}
	return cut
}

func blankCut(lines []string, k int) int {
	choice, fallback := -1, -1
	for i := range k {
		if !blankLine(lines[i]) {
			continue
		}
		fallback = i
		if i+1 < len(lines) && startsAtColumnZero(lines[i+1]) {
			choice = i
		}
	}
	switch {
	case choice >= 0:
		return choice + 1
	case fallback >= 0:
		return fallback + 1
	}
	return k
}

func startsAtColumnZero(line string) bool {
	return len(line) > 1 && line[1] != ' ' && line[1] != '\t'
}

func blankLine(line string) bool {
	return len(line) > 0 && strings.TrimRight(line[1:], "\r\n") == ""
}

func unitsOf(files []fileDiff) []reviewUnit {
	var units []reviewUnit
	for i, f := range files {
		if f.binary {
			units = append(units, reviewUnit{id: len(units), file: i})
			continue
		}
		for _, h := range f.hunks {
			units = append(units, reviewUnit{id: len(units), file: i, hunks: []hunk{h}})
		}
	}
	return units
}

type packer struct {
	task, tool string
	files      []fileDiff
	perFile    []int
	total      int
	closed     [][]placed
	cur        []placed
	err        error
}

type packState struct {
	closed [][]placed
	cur    []placed
}

func (pk *packer) save() packState {
	return packState{closed: slices.Clone(pk.closed), cur: slices.Clone(pk.cur)}
}

func (pk *packer) restore(s packState) {
	pk.closed, pk.cur = s.closed, s.cur
}

func (pk *packer) fits(parts []placed) bool {
	_, size, err := partRequest(pk.task, pk.tool, partFilesOf(pk.files, pk.perFile, parts, true), pk.total, judgePart{Index: reviewMaxParts, Of: reviewMaxParts})
	if err != nil {
		pk.err = err
		return false
	}
	return size <= reviewRequestLimit
}

func (pk *packer) tryAdd(p placed) bool {
	candidate := append(slices.Clone(pk.cur), p)
	if !pk.fits(candidate) {
		return false
	}
	pk.cur = candidate
	return true
}

func (pk *packer) open() bool {
	if len(pk.cur) == 0 {
		return true
	}
	if len(pk.closed)+2 > reviewMaxParts {
		return false
	}
	pk.closed = append(pk.closed, pk.cur)
	pk.cur = nil
	return true
}

func (pk *packer) place(u reviewUnit) (bool, error) {
	whole := placed{unit: u.id, file: u.file, hunks: u.hunks}
	if pk.tryAdd(whole) {
		return true, nil
	}
	if !pk.open() {
		return false, nil
	}
	if pk.tryAdd(whole) {
		return true, nil
	}
	if pk.err != nil || len(u.hunks) != 1 {
		return false, pk.err
	}
	return pk.placePieces(u)
}

func (pk *packer) placePieces(u reviewUnit) (bool, error) {
	pieces, complete := splitHunk(u.hunks[0], func(h hunk) bool {
		return pk.fits([]placed{{unit: u.id, file: u.file, hunks: []hunk{h}, piece: true}})
	})
	if pk.err != nil || !complete {
		return false, pk.err
	}
	for _, piece := range pieces {
		p := placed{unit: u.id, file: u.file, hunks: []hunk{piece}, piece: true}
		if pk.tryAdd(p) {
			continue
		}
		if !pk.open() || !pk.tryAdd(p) {
			return false, pk.err
		}
	}
	return true, nil
}

func partFilesOf(files []fileDiff, perFile []int, parts []placed, measure bool) []partFile {
	var out []partFile
	position := map[int]int{}
	counted := map[int]map[int]bool{}
	partial := map[int]bool{}
	for _, p := range parts {
		pos, ok := position[p.file]
		if !ok {
			pos = len(out)
			position[p.file] = pos
			out = append(out, partFile{index: p.file, file: &files[p.file]})
			counted[p.file] = map[int]bool{}
		}
		out[pos].hunks = append(out[pos].hunks, p.hunks...)
		counted[p.file][p.unit] = true
		partial[p.file] = partial[p.file] || p.piece
	}
	for i := range out {
		idx := out[i].index
		out[i].truncated = !measure && (partial[idx] || len(counted[idx]) < perFile[idx])
	}
	return out
}

func packParts(task, name string, files []fileDiff) ([]judgeInput, int, error) {
	units := unitsOf(files)
	perFile := make([]int, len(files))
	for _, u := range units {
		perFile[u.file]++
	}
	pk := &packer{task: task, tool: name, files: files, perFile: perFile, total: len(units)}
	sent := 0
	for _, u := range units {
		snap := pk.save()
		ok, err := pk.place(u)
		if err != nil {
			return nil, 0, err
		}
		if !ok {
			pk.restore(snap)
			break
		}
		sent++
	}
	if len(pk.cur) > 0 {
		pk.closed = append(pk.closed, pk.cur)
	}
	omitted := len(units) - sent
	inputs := make([]judgeInput, 0, len(pk.closed))
	for i, parts := range pk.closed {
		input, _, err := partRequest(task, name, partFilesOf(files, perFile, parts, false), omitted, judgePart{Index: i + 1, Of: len(pk.closed)})
		if err != nil {
			return nil, 0, err
		}
		inputs = append(inputs, input)
	}
	return inputs, omitted, nil
}

func partRequest(task, name string, files []partFile, omitted int, part judgePart) (judgeInput, int, error) {
	state := judgeState{Task: task, Tool: name, Files: make([]judgeFile, 0, len(files)), Part: part, OmittedHunks: omitted}
	for _, f := range files {
		state.Files = append(state.Files, f.judgeFile())
	}
	raw, err := json.Marshal(state)
	if err != nil {
		return judgeInput{}, 0, err
	}
	questions := reviewQuestionSet(files, task)
	size, err := decide.RequestSize(jsontext.Value(raw), questions)
	if err != nil {
		return judgeInput{}, 0, err
	}
	return judgeInput{state: jsontext.Value(raw), questions: questions}, size, nil
}

func (f partFile) judgeFile() judgeFile {
	return judgeFile{
		Path: f.file.path, Kind: f.file.kind, Change: f.file.change, Added: f.file.added, Removed: f.file.removed,
		Diff: fileText(f.file, f.hunks), Truncated: f.truncated,
	}
}

func fileText(f *fileDiff, hunks []hunk) string {
	if f.binary {
		return f.header
	}
	if len(hunks) == 0 {
		return ""
	}
	var b strings.Builder
	b.WriteString(f.header)
	for _, h := range hunks {
		b.WriteString(h.text())
	}
	return b.String()
}

func reviewQuestionSet(files []partFile, task string) map[string]decide.Question {
	set := map[string]decide.Question{}
	for _, q := range reviewQuestions {
		if q.turn {
			continue
		}
		if q.id != soundID && !editAsks(q, files, task) {
			continue
		}
		set[q.id] = decide.Question{Type: decide.TypeNoul, Instructions: q.instructions, Criteria: map[string]string{"true": q.yes, "false": q.no}}
	}
	return set
}

func editAsks(q reviewQuestion, files []partFile, task string) bool {
	if q.needsTask && task == "" {
		return false
	}
	for _, f := range files {
		if f.file.added == 0 || !slices.Contains(q.kinds, f.file.kind) {
			continue
		}
		if q.concurrent && !f.file.concurrent {
			continue
		}
		return true
	}
	return false
}

func requireMasked(mask func(string) (string, error), state jsontext.Value) error {
	masked, err := mask(string(state))
	if err != nil {
		return errReviewMask
	}
	if masked != string(state) {
		return errReviewSecret
	}
	return nil
}
