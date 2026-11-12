package agent

import (
	"bytes"
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"golang.org/x/sync/errgroup"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/textdiff"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	reviewTimeout            = 8 * time.Second
	reviewFlagThreshold      = 0.9
	reviewUncertainThreshold = 0.5
	reviewSoundThreshold     = 0.5
	reviewFeedbackCap        = 3
	reviewRequestLimit       = 30000
	reviewMaxParts           = 8
	reviewPartWorkers        = 4
	reviewHostRequests       = 8
	reviewLoadLimit          = 4 << 20
	reviewContextLines       = 3
	soundID                  = "sound"
	reviewClean              = "clean"
	reviewUncertain          = "uncertain"
	reviewFlagged            = "flagged"
	reviewError              = "error"
	reviewPathsWithheld      = "[paths withheld: masking failed]"
)

var (
	errReviewInterrupted  = errors.New("review: the turn was interrupted")
	errReviewMask         = errors.New("review: the judge state could not be masked")
	errReviewSecret       = errors.New("review: the judge state held an unmasked secret")
	errReviewNoPart       = errors.New("review: no part fits the request limit")
	reviewBuiltInSpeakers = []string{}
	reviewConcurrency     = regexp.MustCompile(`\bgo\s+\w|\bchan\b|\bsync\.|\batomic\.|\bselect\s*\{|Mutex|WaitGroup|\basync\b|\bawait\b|Promise|\bWorker\b|\bspawn\b|Thread|threading|asyncio|tokio|\bArc<|setTimeout|setInterval`)
	hunkHeader            = regexp.MustCompile(`^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@`)
	allKinds              = []string{"code", "interface", "config", "text"}
)

type reviewQuestion struct {
	id, label, feedback, instructions, yes, no string
	kinds                                      []string
	turn, concurrent, needsTask                bool
}

var reviewQuestions = []reviewQuestion{
	{id: soundID, instructions: "Do the added lines in `files[].diff` serve `task` and show no defect? Judge only the lines shown. A file with `truncated` set to true has lines you cannot see.", yes: "Every added line serves `task`, and no added line shows a defect.", no: "An added line shows a defect, or the diff holds a change that `task` does not ask for.", kinds: allKinds},
	{id: "broken_syntax", label: "broken syntax", feedback: "The added lines may leave a bracket, brace, string, or block unclosed.", instructions: "Do the added lines in `files[].diff` leave the file syntactically broken? Judge only the lines shown. A file with `truncated` set to true has lines you cannot see.", yes: "An added line opens a bracket, brace, parenthesis, string, or block that no shown line closes, or closes one that no shown line opens.", no: "Every bracket, brace, parenthesis, string, and block touched by the added lines opens and closes within the shown lines.", kinds: []string{"code", "interface", "config"}},
	{id: "lost_code", label: "removed code", feedback: "The edit may remove code whose behaviour no added line restores.", instructions: "Does the diff in `files[].diff` remove code whose behaviour no added line restores and that `task` does not ask to remove? Judge only the lines shown.", yes: "A removed line holds a statement, branch, check, function, or test whose behaviour no added line restores, and `task` does not ask for that removal.", no: "Every removed line is rewritten by an added line, appears again elsewhere in the shown diff, or is removed because `task` asks for it.", kinds: allKinds},
	{id: "wrong_logic", label: "wrong logic", feedback: "An added condition, bound, index, return value, or error handling may contradict the lines around it.", instructions: "Does an added line in `files[].diff` contradict the lines shown around it, or drop an error value that it receives? Judge only the lines shown.", yes: "An added line holds a condition, bound, index or return value that contradicts the shown lines around it, or it drops an error value that the same line receives.", no: "Every added condition, bound, index and return value agrees with the shown lines around it, and every received error value is checked or returned.", kinds: []string{"code", "interface"}},
	{id: "placeholder", label: "placeholder", feedback: "An added line may be a placeholder instead of working code.", instructions: "Does an added line in `files[].diff` stand in for code that was not written?", yes: "An added line is a placeholder such as `TODO`, `...`, `pass`, `unimplemented`, an empty function body, or a comment saying existing code stays unchanged.", no: "Every added function body holds working statements, and no added comment says that code was left out.", kinds: allKinds},
	{id: "off_task", label: "off-task change", feedback: "The edit may change code that the task does not ask for.", instructions: "Does the diff in `files[].diff` change code that `task` does not call for?", yes: "A hunk renames, reformats, moves, or rewrites code that neither `task` nor the change `task` asks for needs.", no: "Every hunk changes code that `task` names or that the requested change needs.", kinds: allKinds, needsTask: true},
	{id: "needless_layer", label: "needless layer", feedback: "An added function, type, option, or parameter may only pass its input on, or a block may nest too deeply.", instructions: "Do the added lines in `files[].diff` add a function, type, option, or parameter that does no work of its own, or nest blocks too deeply? Judge only the lines shown.", yes: "The added lines introduce a function, type, option or parameter that the shown lines use in one place and that only passes its input on, or they nest blocks more than three levels deep.", no: "Every added function, type, option and parameter does work of its own or has more than one user in the shown lines, and no added block nests more than three levels deep.", kinds: []string{"code", "interface"}},
	{id: "narration", label: "narration", feedback: "An added comment may repeat the line below it, or an added line may be commented-out code or debug output.", instructions: "Do the added lines in `files[].diff` add comments that describe the change or repeat the line below, or add commented-out code or debug output? Judge only the lines shown.", yes: "An added comment repeats the line below it or describes the change itself, or an added line is commented-out code or leftover debug output.", no: "Every added comment states something the code cannot say, and no added line is commented-out code or debug output.", kinds: []string{"code", "interface"}},
	{id: "unguarded_state", label: "unguarded state", feedback: "An added line may use shared state or start a task without a lock, atomic operation, stop, or wait.", instructions: "Do the added lines in `files[].diff` touch state that two goroutines, threads, or async tasks share, or start a task, without a guard in the lines shown? Judge only the lines shown.", yes: "An added line reads or writes a value that two goroutines, threads or async tasks share, and no shown line guards it; or an added line starts a task that no shown line stops, waits for or cancels.", no: "Every shared value that the added lines touch has a lock, channel or atomic operation in the shown lines, and every started task has a shown stop, wait or cancel.", kinds: []string{"code", "interface"}, concurrent: true},
	{id: "wasted_work", label: "wasted work", feedback: "An added loop may repeat work whose result does not change, or an added read may have no bound.", instructions: "Do the added lines in `files[].diff` repeat work inside a loop, or read input into memory with no bound? Judge only the lines shown.", yes: "An added loop repeats a call, allocation, query or file read whose result is the same in every pass, or an added line reads a whole input into memory with no bound.", no: "Every call inside an added loop depends on the loop's own value, and every added read has a bound in the shown lines.", kinds: []string{"code", "interface"}},
	{id: "unsafe_input", label: "unsafe input", feedback: "An added line may hold a literal credential, or build a command, query, path, or markup from an outside value without binding or escaping it.", instructions: "Do the added lines in `files[].diff` hold a literal credential, or pass an outside value to a command, query, path, or markup without binding or escaping it? Judge only the lines shown.", yes: "An added line holds a literal key, token or password, or it builds a command, query, path or markup from an outside value without the binding or escaping that the shown lines use elsewhere.", no: "No added line holds a literal credential, and every outside value that reaches a command, query, path or markup is bound or escaped in the shown lines.", kinds: []string{"code", "interface", "config"}},
	{id: "unclear_contract", label: "unclear contract", feedback: "An added exported function may choose its behaviour with a flag, or an added name may not say what the code does.", instructions: "Do the added lines in `files[].diff` add an exported function that switches behaviour on a boolean or untyped value, or a name that does not say what the code does? Judge only the lines shown.", yes: "An added exported function takes a boolean or untyped value that selects between behaviours, or an added name (`helper`, `manager`, `util`, `data`, `handle`, `process`) does not say what the code does.", no: "Every added exported function has one behaviour per name, and every added name says what the code does.", kinds: []string{"code", "interface"}},
	{id: "off_token", label: "off-token style", feedback: "An added style may use a literal colour, font, spacing, or radius, add a gradient, or remove a focus outline.", instructions: "Do the added styles in `files[].diff` set literal values where the shown lines use variables, or add a gradient or remove a focus outline? Judge only the lines shown.", yes: "An added style sets a literal colour, font, spacing or radius where the shown lines of that file use variables for that kind of value, or it adds a gradient, or it removes a focus outline.", no: "Every added colour, font, spacing and radius uses a variable where the shown lines do, and no added style adds a gradient or removes a focus outline.", kinds: []string{"interface"}},
	{id: "layout_motion", label: "layout or motion", feedback: "An added style may fix a size in pixels with no narrow-screen rule, or add motion with no reduced-motion rule.", instructions: "Do the added styles in `files[].diff` fix a container's size in pixels or hide overflow with no narrow-screen rule, or add motion with no reduced-motion rule? Judge only the lines shown.", yes: "An added style fixes a container's width or height in pixels or hides overflowing text with no rule for narrow screens in the shown lines, or it adds an animation or transition with no reduced-motion rule, or it transitions `all`.", no: "Every added size bends with its container or has a narrow-screen rule in the shown lines, and every added animation or transition names its properties and has a reduced-motion rule.", kinds: []string{"interface"}},
	{id: "incomplete", label: "incomplete", feedback: "The task may name a change that the diff does not make, or leave a placeholder where the task needs working code.", instructions: "Does the diff in `files[].diff` leave a change that `task` names undone, or leave a placeholder where `task` needs working code? Judge only the lines shown.", yes: "`task` names a change, file, case or test that no hunk of the diff makes, or a hunk leaves a placeholder where `task` needs working code.", no: "Every change that `task` names appears in the diff as working code.", kinds: allKinds, turn: true, needsTask: true},
}

type reviewRound struct {
	group   sync.WaitGroup
	pending []*event.ToolJudged
}

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
}

type unitInput struct {
	parts     []judgeInput
	paths     []string
	truncated bool
	added     int
	err       error
}

type judgment struct {
	by      string
	scores  map[string]float64
	verdict string
	flags   []string
	spoken  []string
	err     error
	failed  int
}

func checkReviewFeedback(ids []string) error {
	for _, id := range ids {
		if id == soundID || !slices.ContainsFunc(reviewQuestions, func(q reviewQuestion) bool { return q.id == id }) {
			return fmt.Errorf("edit_review.feedback: unknown question %s", id)
		}
	}
	return nil
}

func (h *Host) maskReview(text string) (string, error) {
	if h.reviewMask != nil {
		return h.reviewMask(text)
	}
	return h.mask(text)
}

func (w *worker) startReview(ctx context.Context, p *prepared, completed event.ToolCompleted) {
	if w.round == nil || w.t.host.cfg.EditJudge == nil || p.class != tool.ClassEdit || !completed.OK || len(p.files) == 0 {
		return
	}
	task := w.task()
	files := slices.Clone(p.files)
	name, callID := p.call.Name, p.call.ID
	slot := new(event.ToolJudged)
	w.round.pending = append(w.round.pending, slot)
	w.round.group.Go(func() { *slot = w.judgeEdit(ctx, callID, task, name, files) })
}

func (w *worker) joinReviews(ctx context.Context) error {
	round := w.round
	w.round = nil
	if round == nil {
		return nil
	}
	round.group.Wait()
	given, err := w.feedbackGiven()
	if err != nil {
		return err
	}
	var judged []event.Event
	for _, slot := range round.pending {
		if slot.CallID == "" {
			continue
		}
		w.eligible = true
		entry := *slot
		entry.Feedback = entry.Verdict == reviewFlagged && len(entry.Spoken) > 0 && given < reviewFeedbackCap
		if entry.Feedback {
			given++
		} else {
			entry.Spoken = nil
		}
		judged = append(judged, entry)
	}
	if len(judged) == 0 {
		return nil
	}
	_, err = w.t.appendEvents(context.WithoutCancel(ctx), judged...)
	return err
}

func (w *worker) feedbackGiven() (int, error) {
	v, err := w.t.view()
	if err != nil {
		return 0, err
	}
	given := 0
	for _, item := range event.ActivePath(v.records) {
		if judged, ok := item.Event.(event.ToolJudged); ok && judged.Feedback && judged.TurnID == w.turnID {
			given++
		}
	}
	return given, nil
}

func (w *worker) judgeEdit(ctx context.Context, callID, task, name string, changes []event.FileChange) event.ToolJudged {
	h := w.t.host
	tctx, cancel := context.WithTimeoutCause(ctx, h.reviewTimeout, reviewTimeoutCause(h.reviewTimeout))
	defer cancel()
	start := time.Now()
	unit := w.prepareUnit(tctx, task, name, changes)
	if unit.err == nil && unit.added == 0 {
		return event.ToolJudged{}
	}
	if unit.err == nil && len(unit.parts) == 0 {
		unit.err = errReviewNoPart
	}
	if unit.err != nil {
		return w.toolJudged(callID, unit, judgment{by: "judge", err: unit.err}, 0, start)
	}
	j, requests := w.judgeParts(ctx, tctx, unit.parts)
	return w.toolJudged(callID, unit, j, requests, start)
}

func reviewTimeoutCause(d time.Duration) error {
	return fmt.Errorf("review timed out after %s", d)
}

func (w *worker) prepareUnit(ctx context.Context, task, name string, changes []event.FileChange) unitInput {
	h := w.t.host
	paths, err := maskReviewPaths(h, changes)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	files, unit, err := w.loadUnitFiles(ctx, changes, paths)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	diffs, err := diffFiles(files, reviewContextLines)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	parts, omitted, err := packMasked(h, task, name, diffs)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	if omitted > 0 {
		unit.truncated = true
	}
	for _, d := range diffs {
		unit.added += d.added
	}
	unit.parts, unit.paths = parts, paths
	return unit
}

func maskReviewPaths(h *Host, changes []event.FileChange) ([]string, error) {
	paths := make([]string, 0, len(changes))
	for _, change := range changes {
		masked, err := h.maskReview(change.Path)
		if err != nil {
			return []string{reviewPathsWithheld}, errReviewMask
		}
		paths = append(paths, masked)
	}
	return paths, nil
}

func packMasked(h *Host, task, name string, diffs []fileDiff) ([]judgeInput, int, error) {
	parts, omitted, err := packParts(task, name, diffs)
	if err != nil {
		return nil, 0, err
	}
	for _, part := range parts {
		if err := requireMasked(h.maskReview, part.state); err != nil {
			return nil, 0, err
		}
	}
	return parts, omitted, nil
}

func (w *worker) loadUnitFiles(ctx context.Context, changes []event.FileChange, paths []string) ([]reviewFile, unitInput, error) {
	var unit unitInput
	files := make([]reviewFile, 0, len(changes))
	loaded := 0
	for i, change := range changes {
		file, err := w.loadReviewFile(ctx, change, paths[i])
		if err != nil {
			return nil, unit, err
		}
		files = append(files, file)
		loaded += len(file.pre) + len(file.post)
		if loaded >= reviewLoadLimit && i+1 < len(changes) {
			unit.truncated = true
			break
		}
	}
	for _, file := range files {
		if file.skip == "large" {
			unit.truncated = true
		}
	}
	return files, unit, nil
}

func (w *worker) loadReviewFile(ctx context.Context, change event.FileChange, path string) (reviewFile, error) {
	h := w.t.host
	var pre, post []byte
	if change.PreBlob != nil {
		data, err := h.journal.Blob(change.PreBlob.Blob)
		if err != nil {
			return reviewFile{}, fmt.Errorf("review: read pre-image of %s: %w", path, err)
		}
		pre = data
	}
	if change.PostSHA != "" {
		raw, err := w.t.runner.ReadFile(ctx, absPath(w.t.created.Cwd, change.Path))
		if err != nil {
			return reviewFile{}, fmt.Errorf("review: read %s: %w", path, err)
		}
		sum := sha256.Sum256(raw)
		if hex.EncodeToString(sum[:]) != change.PostSHA {
			return reviewFile{}, fmt.Errorf("review: %s changed before it was judged", path)
		}
		masked, err := h.maskReview(string(raw))
		if err != nil {
			return reviewFile{}, errReviewMask
		}
		post = []byte(masked)
	}
	return classifyReviewFile(path, reviewChange(change), pre, post), nil
}

func reviewChange(change event.FileChange) string {
	switch {
	case change.PreSHA == "":
		return "add"
	case change.PostSHA == "":
		return "delete"
	}
	return "update"
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

func (w *worker) judgeParts(ctx, tctx context.Context, parts []judgeInput) (judgment, int) {
	client, name, err := w.t.host.cfg.EditJudge(tctx, w.t.id)
	if err != nil {
		return judgment{by: "judge", err: reviewFailure(ctx, tctx, err)}, 0
	}
	by := "judge:" + name
	results := make([]judgment, len(parts))
	var group errgroup.Group
	group.SetLimit(reviewPartWorkers)
	for i, part := range parts {
		group.Go(func() error {
			results[i] = w.askPart(ctx, tctx, client, by, part)
			return nil
		})
	}
	_ = group.Wait()
	return combineParts(results), len(parts)
}

func (w *worker) askPart(ctx, tctx context.Context, client decide.Client, by string, part judgeInput) judgment {
	h := w.t.host
	select {
	case h.reviewSlots <- struct{}{}:
	case <-tctx.Done():
		return judgment{by: by, err: reviewFailure(ctx, tctx, nil)}
	}
	defer func() { <-h.reviewSlots }()
	answers, err := client.Ask(tctx, part.state, part.questions)
	if err != nil {
		return judgment{by: by, err: reviewFailure(ctx, tctx, err)}
	}
	scores, err := scoreAnswers(answers, part.questions)
	if err != nil {
		return judgment{by: by, err: err}
	}
	verdict, flags := reviewVerdict(scores)
	return judgment{by: by, scores: scores, verdict: verdict, flags: flags, spoken: reviewSpoken(scores, flags, h.cfg.EditFeedback)}
}

func reviewFailure(ctx, tctx context.Context, err error) error {
	switch {
	case ctx.Err() != nil:
		return errReviewInterrupted
	case tctx.Err() != nil:
		return context.Cause(tctx)
	}
	return err
}

func reviewVerdict(scores map[string]float64) (string, []string) {
	var flags []string
	uncertain := false
	for _, q := range reviewQuestions {
		score, ok := scores[q.id]
		if !ok || q.id == soundID {
			continue
		}
		switch {
		case score >= reviewFlagThreshold:
			flags = append(flags, q.id)
		case score >= reviewUncertainThreshold:
			uncertain = true
		}
	}
	switch {
	case len(flags) > 0:
		return reviewFlagged, flags
	case uncertain:
		return reviewUncertain, nil
	}
	return reviewClean, nil
}

func reviewSpoken(scores map[string]float64, flags, feedback []string) []string {
	sound, ok := scores[soundID]
	if !ok || sound >= reviewSoundThreshold {
		return nil
	}
	speakers := feedback
	if speakers == nil {
		speakers = reviewBuiltInSpeakers
	}
	var spoken []string
	for _, flag := range flags {
		if slices.Contains(speakers, flag) {
			spoken = append(spoken, flag)
		}
	}
	return spoken
}

func combineParts(parts []judgment) judgment {
	out := judgment{by: parts[0].by}
	var answered []judgment
	var firstErr error
	for i, part := range parts {
		if part.err != nil {
			out.failed++
			if firstErr == nil {
				firstErr = fmt.Errorf("part %d of %d: %w", i+1, len(parts), part.err)
			}
			continue
		}
		answered = append(answered, part)
	}
	if len(answered) == 0 {
		out.err = firstErr
		return out
	}
	out.verdict = worstVerdict(answered)
	if out.failed > 0 && out.verdict == reviewClean {
		out.verdict = reviewUncertain
	}
	out.scores = mergeScores(answered)
	out.flags = unionOrdered(answered, func(j judgment) []string { return j.flags })
	out.spoken = unionOrdered(answered, func(j judgment) []string { return j.spoken })
	return out
}

func worstVerdict(parts []judgment) string {
	worst := reviewClean
	for _, part := range parts {
		switch part.verdict {
		case reviewFlagged:
			return reviewFlagged
		case reviewUncertain:
			worst = reviewUncertain
		}
	}
	return worst
}

func mergeScores(parts []judgment) map[string]float64 {
	out := map[string]float64{}
	for _, part := range parts {
		for name, score := range part.scores {
			prev, seen := out[name]
			switch {
			case !seen:
				out[name] = score
			case name == soundID:
				out[name] = min(prev, score)
			default:
				out[name] = max(prev, score)
			}
		}
	}
	return out
}

func unionOrdered(parts []judgment, pick func(judgment) []string) []string {
	seen := map[string]bool{}
	for _, part := range parts {
		for _, name := range pick(part) {
			seen[name] = true
		}
	}
	var out []string
	for _, q := range reviewQuestions {
		if seen[q.id] {
			out = append(out, q.id)
		}
	}
	return out
}

func (w *worker) toolJudged(callID string, unit unitInput, j judgment, requests int, start time.Time) event.ToolJudged {
	entry := event.ToolJudged{
		CallID: callID, TurnID: w.turnID, By: j.by, Paths: capPaths(unit.paths), Parts: cmp.Or(requests, 1),
		Truncated: unit.truncated || j.failed > 0, ElapsedMS: time.Since(start).Milliseconds(),
	}
	if j.failed > 0 {
		entry.FailedParts = j.failed
	}
	if j.err != nil {
		entry.Verdict = reviewError
		entry.Error = w.reviewErrorText(j.err)
		return entry
	}
	entry.Verdict, entry.Scores, entry.Flags, entry.Spoken = j.verdict, j.scores, j.flags, j.spoken
	if entry.Truncated && entry.Verdict == reviewClean {
		entry.Verdict = reviewUncertain
	}
	return entry
}

func capPaths(paths []string) []string {
	if len(paths) > event.MaxJudgedPaths {
		return paths[:event.MaxJudgedPaths]
	}
	return paths
}

func (w *worker) reviewErrorText(err error) string {
	text, maskErr := w.t.host.maskReview(truncate(err.Error(), event.MaxJudgeErrorBytes))
	if maskErr != nil {
		return "review: the error text could not be masked"
	}
	return text
}
