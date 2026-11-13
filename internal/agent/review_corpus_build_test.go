package agent

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	json "encoding/json/v2"
	"fmt"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/textdiff"
)

const corpusRepo = "../.."

type statusEntry struct{ status, path string }

type commitRow struct {
	sha, parent, subject string
	entries              []statusEntry
}

type repoFile struct {
	path, change string
	pre, post    []byte
}

type mutation struct {
	path, kind, task string
	lines            []string
	added            map[int]bool
	endsNL           bool
}

var (
	elision       = strings.Repeat(".", 3)
	placeholder   = elision + " existing code " + elision
	closers       = []string{"}", "]", ")", "/>"}
	lockLine      = regexp.MustCompile(`\.Lock\(\)|\.Unlock\(\)|atomic\.|<-`)
	callLine      = regexp.MustCompile(`^\t+[A-Za-z_.]+\(.*\)$`)
	funcDecl      = regexp.MustCompile(`^func ([A-Za-z_]\w*)\(`)
	varColor      = regexp.MustCompile(`var\(--[\w-]+\)`)
	codeStatement = regexp.MustCompile(`^\t+(return |[A-Za-z_.]+ :?= |[A-Za-z_.]+\(.*\)$)`)
	cssDecl       = regexp.MustCompile(`^\s+[a-zA-Z-]+:\s*[^;{}]+;$`)
	configKey     = regexp.MustCompile(`^\s*("[^"]+"|[A-Za-z_][\w.-]*)\s*[:=]\s*\S`)
	sentence      = regexp.MustCompile(`^[A-Z].*[.!?]$`)
	offTaskNames  = map[string]*regexp.Regexp{
		"code":      regexp.MustCompile(`^\s*([a-z][A-Za-z0-9_]*) :=`),
		"interface": regexp.MustCompile(`\.([a-z][a-zA-Z0-9_-]*)\s*\{`),
		"config":    regexp.MustCompile(`^\s*"?([A-Za-z_][\w-]*)"?\s*[:=]`),
		"text":      regexp.MustCompile(`^#+ ([A-Za-z][\w-]*)`),
	}
	logicOps = [][2]string{{"==", "!="}, {"!=", "=="}, {"<=", "<"}, {">=", ">"}, {"&&", "||"}, {"||", "&&"}}
)

var defectMutators = map[string]func(*mutation) bool{
	"broken_syntax":    mutBrokenSyntax,
	"lost_code":        mutLostCode,
	"wrong_logic":      mutWrongLogic,
	"placeholder":      mutPlaceholder,
	"off_task":         mutOffTask,
	"needless_layer":   mutNeedlessLayer,
	"narration":        mutNarration,
	"unguarded_state":  mutUnguardedState,
	"wasted_work":      mutWastedWork,
	"unsafe_input":     mutUnsafeInput,
	"unclear_contract": mutUnclearContract,
	"off_token":        mutOffToken,
	"layout_motion":    mutLayoutMotion,
}

func gitRun(t testing.TB, args ...string) []byte {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-C", corpusRepo}, args...)...)
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git %v: %v", args, err)
	}
	return out
}

func listCommits(t testing.TB) []commitRow {
	t.Helper()
	out := gitRun(t, "log", "--no-merges", "--no-renames", "--name-status", "--format=COMMIT%x09%H%x09%P%x09%s", "3c4b6f94")
	var rows []commitRow
	for line := range strings.SplitSeq(string(out), "\n") {
		if head, ok := strings.CutPrefix(line, "COMMIT\t"); ok {
			f := strings.SplitN(head, "\t", 3)
			if len(f) < 3 {
				t.Fatalf("malformed commit line %q", line)
			}
			rows = append(rows, commitRow{sha: f[0], parent: firstWord(f[1]), subject: f[2]})
			continue
		}
		if status, path, ok := strings.Cut(line, "\t"); ok && len(rows) > 0 {
			last := &rows[len(rows)-1]
			last.entries = append(last.entries, statusEntry{status: status, path: path})
		}
	}
	slices.SortFunc(rows, func(a, b commitRow) int { return strings.Compare(hashHex(a.sha), hashHex(b.sha)) })
	return rows
}

func firstWord(s string) string {
	fields := strings.Fields(s)
	if len(fields) == 0 {
		return ""
	}
	return fields[0]
}

func gitBlob(t testing.TB, rev, path string) []byte {
	return gitRun(t, "cat-file", "blob", rev+":"+path)
}

func loadCommit(t testing.TB, c commitRow) ([]repoFile, bool) {
	t.Helper()
	files := make([]repoFile, 0, len(c.entries))
	for _, e := range c.entries {
		if e.path == "" || strings.HasPrefix(e.path, `"`) || !slices.Contains([]string{"A", "M", "D"}, e.status) {
			return nil, false
		}
		f := repoFile{path: e.path}
		if e.status != "A" {
			f.pre = gitBlob(t, c.parent, e.path)
		}
		if e.status != "D" {
			f.post = gitBlob(t, c.sha, e.path)
		}
		f.change = corpusChange(corpusFile{Pre: string(f.pre), Post: string(f.post)})
		files = append(files, f)
	}
	return files, len(files) > 0
}

func reviewFilesOf(files []repoFile) []reviewFile {
	out := make([]reviewFile, 0, len(files))
	for _, f := range files {
		out = append(out, classifyReviewFile(f.path, f.change, f.pre, f.post))
	}
	return out
}

func soundFiles(t testing.TB, files []repoFile) bool {
	t.Helper()
	rf := reviewFilesOf(files)
	for _, f := range rf {
		if f.skip != "" {
			return false
		}
	}
	diffs, err := diffFiles(rf, reviewContextLines)
	if err != nil {
		t.Fatalf("diff: %v", err)
	}
	total := 0
	for _, d := range diffs {
		total += d.added
	}
	return len(files) > 0 && total > 0
}

func turnSubject(c commitRow) bool {
	for _, e := range c.entries {
		if namesFile(c.subject, fileBase(e.path)) {
			return true
		}
	}
	return false
}

func fileBase(path string) string {
	return strings.TrimSuffix(filepath.Base(path), filepath.Ext(path))
}

func namesFile(subject, base string) bool {
	if base == "" {
		return false
	}
	return regexp.MustCompile(`\b` + regexp.QuoteMeta(base) + `\b`).MatchString(subject)
}

func newCase(id, group string, c commitRow, files []repoFile) corpusCase {
	out := corpusCase{ID: id, Group: group, Unit: corpusUnit(group), Source: c.sha, Task: c.subject, Flags: corpusFlags(group)}
	for _, f := range files {
		out.Files = append(out.Files, corpusFile{Path: f.path, Kind: reviewKind(f.path), Pre: string(f.pre), Post: string(f.post)})
	}
	return out
}

func numbered(prefix string, n int) string {
	return fmt.Sprintf("%s-%03d", prefix, n)
}

type soundPool struct {
	room map[string]int
	out  []corpusCase
}

func (p *soundPool) take(c commitRow, f repoFile) {
	p.room[reviewKind(f.path)]--
	p.out = append(p.out, newCase(numbered(groupSoundEdit, len(p.out)+1), groupSoundEdit, c, []repoFile{f}))
}

func pickSoundEdit(t testing.TB, commits []commitRow, used map[string]bool) []corpusCase {
	pool := &soundPool{room: maps.Clone(corpusKindQuota)}
	for _, c := range commits {
		if len(c.entries) != 1 || used[c.sha] {
			continue
		}
		files, ok := loadCommit(t, c)
		if !ok || !soundFiles(t, files) || pool.room[reviewKind(files[0].path)] == 0 {
			continue
		}
		used[c.sha] = true
		pool.take(c, files[0])
	}
	fillSoundEdit(t, commits, pool)
	if len(pool.out) != corpusGroupSizes()[groupSoundEdit] {
		t.Fatalf("sound edit pool yielded %d cases, room left %v", len(pool.out), pool.room)
	}
	return pool.out
}

func fillSoundEdit(t testing.TB, commits []commitRow, pool *soundPool) {
	for _, c := range commits {
		if len(c.entries) < 2 || !hasRoom(pool.room) {
			continue
		}
		files, ok := loadCommit(t, c)
		if !ok {
			continue
		}
		for _, f := range files {
			if pool.room[reviewKind(f.path)] == 0 || !soundFiles(t, []repoFile{f}) {
				continue
			}
			pool.take(c, f)
		}
	}
}

func hasRoom(room map[string]int) bool {
	return slices.ContainsFunc(slices.Collect(maps.Values(room)), func(n int) bool { return n > 0 })
}

func pickSoundTurn(t testing.TB, commits []commitRow, used map[string]bool) []corpusCase {
	var out []corpusCase
	for _, c := range commits {
		if len(out) == corpusGroupSizes()[groupSoundTurn] {
			break
		}
		if used[c.sha] || !turnSubject(c) {
			continue
		}
		files, ok := loadCommit(t, c)
		if !ok || !soundFiles(t, files) {
			continue
		}
		used[c.sha] = true
		out = append(out, newCase(numbered(groupSoundTurn, len(out)+1), groupSoundTurn, c, files))
	}
	if len(out) != corpusGroupSizes()[groupSoundTurn] {
		t.Fatalf("sound turn pool yielded %d cases", len(out))
	}
	return out
}

func pickGroup(t testing.TB, commits []commitRow, group string, used map[string]bool, eligible func(c commitRow, files []repoFile) bool) []corpusCase {
	var out []corpusCase
	for _, c := range commits {
		if len(out) == corpusGroupSizes()[group] {
			break
		}
		if used[c.sha] {
			continue
		}
		files, ok := loadCommit(t, c)
		if !ok || !soundFiles(t, files) || !eligible(c, files) {
			continue
		}
		used[c.sha] = true
		out = append(out, newCase(numbered(group, len(out)+1), group, c, files))
	}
	if len(out) != corpusGroupSizes()[group] {
		t.Fatalf("%s pool yielded %d cases", group, len(out))
	}
	return out
}

func pickMultipart(t testing.TB, commits []commitRow, used map[string]bool) []corpusCase {
	return pickGroup(t, commits, groupMultipart, used, multipartEligible)
}

func multipartEligible(c commitRow, files []repoFile) bool {
	parts, omitted, err := buildReview(c.subject, corpusTool, reviewFilesOf(files))
	return err == nil && len(parts) >= 2 && omitted == 0
}

func pickMoved(t testing.TB, commits []commitRow, used map[string]bool) []corpusCase {
	return pickGroup(t, commits, groupMoved, used, movedEligible)
}

func movedEligible(c commitRow, files []repoFile) bool {
	parts, _, err := buildReview(c.subject, corpusTool, reviewFilesOf(files))
	if err != nil {
		return false
	}
	ref, ok := movedFunction(parts)
	return ok && ref.removed != ref.added
}

func newMutation(path, kind, task string, pre, post []byte) (*mutation, error) {
	diff, _, err := textdiff.Unified("a", "b", pre, post, 0)
	if err != nil {
		return nil, fmt.Errorf("diff %s: %w", path, err)
	}
	_, hunks, err := splitHunks(diff)
	if err != nil {
		return nil, err
	}
	added := map[int]bool{}
	for _, h := range hunks {
		line := h.newStart
		for _, l := range h.lines {
			switch {
			case strings.HasPrefix(l, "+"):
				added[line-1] = true
				line++
			case strings.HasPrefix(l, "-"), strings.HasPrefix(l, `\`):
			default:
				line++
			}
		}
	}
	var lines []string
	if len(post) > 0 {
		lines = strings.Split(strings.TrimSuffix(string(post), "\n"), "\n")
	}
	return &mutation{path: path, kind: kind, task: task, lines: lines, added: added, endsNL: bytes.HasSuffix(post, []byte("\n"))}, nil
}

func (m *mutation) bytes() []byte {
	s := strings.Join(m.lines, "\n")
	if m.endsNL && len(m.lines) > 0 {
		s += "\n"
	}
	return []byte(s)
}

func (m *mutation) in(kinds ...string) bool {
	return slices.Contains(kinds, m.kind)
}

func (m *mutation) find(last bool, pred func(i int, line string) bool) (int, bool) {
	for k := range m.lines {
		i := k
		if last {
			i = len(m.lines) - 1 - k
		}
		if pred(i, m.lines[i]) {
			return i, true
		}
	}
	return -1, false
}

func (m *mutation) touched() map[int]bool {
	out := map[int]bool{}
	for i := range m.added {
		for j := i - 3; j <= i+3; j++ {
			out[j] = true
		}
	}
	return out
}

func (m *mutation) remove(i int) {
	m.lines = slices.Delete(m.lines, i, i+1)
}

func (m *mutation) insert(i int, text string) {
	m.lines = slices.Insert(m.lines, i, text)
}

func indentOf(l string) string {
	return l[:len(l)-len(strings.TrimLeft(l, " \t"))]
}

func (m *mutation) addedNonBlank(i int, l string) bool {
	return m.added[i] && strings.TrimSpace(l) != ""
}

func (m *mutation) statement(l string) bool {
	switch m.kind {
	case "code":
		return codeStatement.MatchString(l) && !strings.HasSuffix(l, "{")
	case "interface":
		return cssDecl.MatchString(l)
	case "config":
		return configKey.MatchString(l)
	}
	return sentence.MatchString(l)
}

func mutBrokenSyntax(m *mutation) bool {
	if !m.in("code", "interface", "config") {
		return false
	}
	i, ok := m.find(true, func(i int, l string) bool {
		return m.added[i] && slices.Contains(closers, strings.TrimSpace(l))
	})
	if ok {
		m.remove(i)
	}
	return ok
}

func mutLostCode(m *mutation) bool {
	touched := m.touched()
	i, ok := m.find(false, func(i int, l string) bool {
		return !touched[i] && strings.TrimSpace(l) != "" && m.statement(l)
	})
	if ok {
		m.remove(i)
	}
	return ok
}

func mutWrongLogic(m *mutation) bool {
	if !m.in("code", "interface") {
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool {
		return m.added[i] && firstLogicOp(l) != nil
	})
	if !ok {
		return false
	}
	op := firstLogicOp(m.lines[i])
	m.lines[i] = strings.Replace(m.lines[i], op[0], op[1], 1)
	return true
}

func firstLogicOp(l string) []string {
	best, at := []string(nil), -1
	for _, pair := range logicOps {
		k := strings.Index(l, pair[0])
		if k >= 0 && (at < 0 || k < at) {
			best, at = []string{pair[0], pair[1]}, k
		}
	}
	return best
}

func mutPlaceholder(m *mutation) bool {
	i, ok := m.find(false, func(i int, l string) bool { return m.addedNonBlank(i, l) })
	if !ok {
		return false
	}
	m.lines[i] = indentOf(m.lines[i]) + m.placeholderLine()
	return true
}

func (m *mutation) placeholderLine() string {
	switch {
	case m.kind == "code":
		return "// " + placeholder
	case m.kind == "interface":
		return "/* " + placeholder + " */"
	case m.kind == "config" && filepath.Ext(m.path) == ".json":
		return `"` + elision + `": "` + placeholder + `"`
	case m.kind == "config":
		return "# " + placeholder
	}
	return placeholder
}

func mutOffTask(m *mutation) bool {
	re := offTaskNames[m.kind]
	if m.task == "" || re == nil {
		return false
	}
	touched := m.touched()
	for i, l := range m.lines {
		sub := re.FindStringSubmatch(l)
		if touched[i] || sub == nil || m.usedByAdded(sub[1]) {
			continue
		}
		pat, repl := m.renamePattern(sub[1])
		for k := range m.lines {
			m.lines[k] = pat.ReplaceAllString(m.lines[k], repl)
		}
		return true
	}
	return false
}

func (m *mutation) usedByAdded(name string) bool {
	re := regexp.MustCompile(`\b` + regexp.QuoteMeta(name) + `\b`)
	for i, l := range m.lines {
		if m.added[i] && re.MatchString(l) {
			return true
		}
	}
	return false
}

func (m *mutation) renamePattern(name string) (*regexp.Regexp, string) {
	q := regexp.QuoteMeta(name)
	if m.kind == "interface" {
		return regexp.MustCompile(`\.(` + q + `)\b`), ".${1}Value"
	}
	return regexp.MustCompile(`\b(` + q + `)\b`), "${1}Value"
}

type funcSig struct {
	name, params, args, result string
}

var (
	funcSigLine = regexp.MustCompile(`^func ([A-Za-z_]\w*)\(([^(){}]*)\)(?: ([^(){}]+))? \{$`)
	identName   = regexp.MustCompile(`^[A-Za-z_]\w*$`)
)

func parseFuncSig(line string) (funcSig, bool) {
	sub := funcSigLine.FindStringSubmatch(line)
	if sub == nil {
		return funcSig{}, false
	}
	names, ok := paramNames(sub[2])
	if !ok {
		return funcSig{}, false
	}
	return funcSig{name: sub[1], params: sub[2], args: strings.Join(names, ", "), result: sub[3]}, true
}

func paramNames(params string) ([]string, bool) {
	if params == "" {
		return nil, true
	}
	var names []string
	for p := range strings.SplitSeq(params, ", ") {
		name, typ, ok := strings.Cut(p, " ")
		if !ok || typ == "" || strings.Contains(typ, "...") || !identName.MatchString(name) {
			return nil, false
		}
		names = append(names, name)
	}
	return names, true
}

func (s funcSig) wrapper(layer string) string {
	head := "func " + layer + "(" + s.params + ")"
	body := s.name + "(" + s.args + ")"
	if s.result != "" {
		head += " " + s.result
		body = "return " + body
	}
	return head + " {\n\t" + body + "\n}"
}

func callOf(name string) *regexp.Regexp {
	return regexp.MustCompile(`\b` + regexp.QuoteMeta(name) + `\(`)
}

func (m *mutation) layerTarget() (funcSig, int, bool) {
	touched := m.touched()
	joined := strings.Join(m.lines, "\n")
	for k, l := range m.lines {
		sig, ok := parseFuncSig(l)
		if !ok || touched[k] || strings.Contains(joined, "func "+sig.name+"Layer(") {
			continue
		}
		call := callOf(sig.name)
		i, found := m.find(false, func(i int, line string) bool {
			return m.added[i] && !strings.HasPrefix(line, "func ") && call.MatchString(line)
		})
		if found {
			return sig, i, true
		}
	}
	return funcSig{}, 0, false
}

func mutNeedlessLayer(m *mutation) bool {
	if !m.in("code") {
		return false
	}
	sig, i, ok := m.layerTarget()
	if !ok {
		return false
	}
	layer := sig.name + "Layer"
	m.lines[i] = callOf(sig.name).ReplaceAllString(m.lines[i], layer+"(")
	m.lines = append(m.lines, "", sig.wrapper(layer))
	return true
}

func mutNarration(m *mutation) bool {
	if !m.in("code", "interface") {
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool { return m.addedNonBlank(i, l) })
	if !ok {
		return false
	}
	text := strings.TrimSpace(m.lines[i])
	comment := "// " + text
	if m.kind == "interface" {
		comment = "/* " + text + " */"
	}
	m.insert(i, indentOf(m.lines[i])+comment)
	return true
}

func (m *mutation) addedLockFree() bool {
	for i, l := range m.lines {
		if m.added[i] && lockLine.MatchString(l) {
			return false
		}
	}
	return true
}

func (m *mutation) concurrentAdded() bool {
	for i, l := range m.lines {
		if m.added[i] && reviewConcurrency.MatchString(l) {
			return true
		}
	}
	return false
}

func mutUnguardedState(m *mutation) bool {
	if !m.in("code") || !m.concurrentAdded() || !m.addedLockFree() {
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool { return !m.added[i] && lockLine.MatchString(l) })
	if ok {
		m.remove(i)
	}
	return ok
}

func mutWastedWork(m *mutation) bool {
	if !m.in("code") {
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool { return m.added[i] && callLine.MatchString(l) })
	if !ok {
		return false
	}
	indent := indentOf(m.lines[i])
	inner := m.lines[i][len(indent):]
	m.lines = slices.Replace(m.lines, i, i+1, indent+"for i := 0; i < 3; i++ {", indent+"\t"+inner, indent+"}")
	return true
}

func mutUnsafeInput(m *mutation) bool {
	var literal string
	switch {
	case m.kind == "code":
		literal = `apiKey = "sk-test-0123456789ab"`
	case m.kind == "config" && filepath.Ext(m.path) != ".json":
		literal = `api_key = "sk-test-0123456789ab"`
	default:
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool { return m.addedNonBlank(i, l) })
	if ok {
		m.insert(i+1, indentOf(m.lines[i])+literal)
	}
	return ok
}

func mutUnclearContract(m *mutation) bool {
	if !m.in("code") {
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool { return m.added[i] && funcDecl.MatchString(l) })
	if !ok {
		return false
	}
	name := funcDecl.FindStringSubmatch(m.lines[i])[1]
	if name == "handle" {
		return false
	}
	re := regexp.MustCompile(`\b` + regexp.QuoteMeta(name) + `\b`)
	for k := range m.lines {
		m.lines[k] = re.ReplaceAllLiteralString(m.lines[k], "handle")
	}
	return true
}

func mutOffToken(m *mutation) bool {
	if !m.in("interface") {
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool { return m.added[i] && varColor.MatchString(l) })
	if !ok {
		return false
	}
	loc := varColor.FindStringIndex(m.lines[i])
	m.lines[i] = m.lines[i][:loc[0]] + "#3366cc" + m.lines[i][loc[1]:]
	return true
}

func mutLayoutMotion(m *mutation) bool {
	if !m.in("interface") {
		return false
	}
	i, ok := m.find(false, func(i int, l string) bool {
		return m.added[i] && strings.HasSuffix(strings.TrimSpace(l), "{")
	})
	if ok {
		m.insert(i+1, "  transition: all 0.3s;")
	}
	return ok
}

func incompleteFiles(t testing.TB, files []repoFile, subject string) ([]repoFile, bool) {
	t.Helper()
	out := slices.Clone(files)
	hit := false
	for i := range out {
		if !hit && namesFile(subject, fileBase(out[i].path)) {
			hit = true
			out[i].post = bytes.Clone(out[i].pre)
			out[i].change = corpusChange(corpusFile{Pre: string(out[i].pre), Post: string(out[i].post)})
		}
	}
	return out, hit && len(out) > 0 && soundFiles(t, out)
}

type defectRun struct {
	counts map[string]int
	out    []corpusCase
}

func (d *defectRun) take(id string, c corpusCase) {
	d.counts[id]++
	c.ID = numbered(id, d.counts[id])
	d.out = append(d.out, c)
}

func (d *defectRun) full(id string) bool {
	return d.counts[id] == corpusGroupSizes()[id]
}

func (d *defectRun) editDefects(t testing.TB, c commitRow, files []repoFile) {
	t.Helper()
	pre, post := files[0].pre, files[0].post
	path, kind := files[0].path, reviewKind(files[0].path)
	for _, id := range corpusDefectIDs {
		if id == "incomplete" || d.full(id) {
			continue
		}
		m, err := newMutation(path, kind, c.subject, pre, post)
		if err != nil {
			t.Fatalf("mutation %s: %v", path, err)
		}
		if !defectMutators[id](m) || bytes.Equal(m.bytes(), post) {
			continue
		}
		mutated := newCase("", id, c, []repoFile{{path: path, change: files[0].change, pre: pre, post: m.bytes()}})
		d.take(id, mutated)
	}
}

func (d *defectRun) turnDefect(t testing.TB, c commitRow, files []repoFile) {
	t.Helper()
	if d.full("incomplete") || !turnSubject(c) {
		return
	}
	kept, ok := incompleteFiles(t, files, c.subject)
	if !ok {
		return
	}
	d.take("incomplete", newCase("", "incomplete", c, kept))
}

func pickDefects(t testing.TB, commits []commitRow) []corpusCase {
	run := &defectRun{counts: map[string]int{}}
	for _, c := range commits {
		if len(run.out) == defectQuotaTotal() {
			break
		}
		files, ok := loadCommit(t, c)
		if !ok {
			continue
		}
		if len(files) == 1 && soundFiles(t, files) {
			run.editDefects(t, c, files)
		}
		if soundFiles(t, files) {
			run.turnDefect(t, c, files)
		}
	}
	for _, id := range corpusDefectIDs {
		if !run.full(id) {
			t.Fatalf("defect %s yielded %d cases", id, run.counts[id])
		}
	}
	return run.out
}

func groupRank(group string) int {
	order := []string{groupSoundEdit, groupSoundTurn, groupMultipart, groupMoved}
	if i := slices.Index(order, group); i >= 0 {
		return i
	}
	return len(order) + slices.Index(corpusDefectIDs, group)
}

func assignSplits(cases []corpusCase) {
	byGroup := groupCases(cases)
	for group, members := range byGroup {
		ids := make([]string, 0, len(members))
		for _, c := range members {
			ids = append(ids, c.ID)
		}
		splits := expectedSplits(ids)
		for i := range cases {
			if cases[i].Group == group {
				cases[i].Split = splits[cases[i].ID]
			}
		}
	}
}

func buildCorpus(t testing.TB) []corpusCase {
	t.Helper()
	commits := listCommits(t)
	used := map[string]bool{}
	var cases []corpusCase
	cases = append(cases, pickSoundEdit(t, commits, used)...)
	cases = append(cases, pickMoved(t, commits, used)...)
	cases = append(cases, pickSoundTurn(t, commits, used)...)
	cases = append(cases, pickMultipart(t, commits, used)...)
	cases = append(cases, pickDefects(t, commits)...)
	assignSplits(cases)
	slices.SortStableFunc(cases, func(a, b corpusCase) int {
		if d := groupRank(a.Group) - groupRank(b.Group); d != 0 {
			return d
		}
		return strings.Compare(a.ID, b.ID)
	})
	return cases
}

func writeCorpus(t testing.TB, cases []corpusCase) []byte {
	t.Helper()
	var buf bytes.Buffer
	for _, c := range cases {
		line, err := json.Marshal(c)
		if err != nil {
			t.Fatalf("encode case %s: %v", c.ID, err)
		}
		buf.Write(line)
		buf.WriteByte('\n')
	}
	if err := os.WriteFile(corpusCasesPath, buf.Bytes(), 0o644); err != nil {
		t.Fatalf("write corpus: %v", err)
	}
	return buf.Bytes()
}

func writeRatchet(t testing.TB, corpus []byte) {
	t.Helper()
	sum := sha256.Sum256(corpus)
	r := ratchetFile{CorpusSHA256: hex.EncodeToString(sum[:]), Judge: corpusJudge, Threshold: corpusThreshold}
	if raw, err := os.ReadFile(corpusRatchetPath); err == nil {
		var prev ratchetFile
		if json.Unmarshal(raw, &prev) == nil && prev.CorpusSHA256 == r.CorpusSHA256 && prev.Judge == r.Judge && prev.Threshold == r.Threshold {
			r.Measured = prev.Measured
		}
	}
	out, err := marshalRatchet(r)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(corpusRatchetPath, out, 0o644); err != nil {
		t.Fatalf("write ratchet: %v", err)
	}
}

func TestEditReviewCorpusBuild(t *testing.T) {
	if os.Getenv("DALGONA_EDIT_REVIEW_BUILD") != "1" {
		t.Skip("set DALGONA_EDIT_REVIEW_BUILD=1 to regenerate the corpus from history")
	}
	corpus := writeCorpus(t, buildCorpus(t))
	writeRatchet(t, corpus)
}

func TestTurnSubjectNamesFileAsWholeWord(t *testing.T) {
	cases := []struct {
		name    string
		subject string
		path    string
		want    bool
	}{
		{name: "scope inside scopes", subject: "fix the scopes list", path: "x/scope.go", want: false},
		{name: "proc inside process", subject: "process the items", path: "x/proc.go", want: false},
		{name: "model inside models", subject: "update the models", path: "x/model.go", want: false},
		{name: "whole word", subject: "update model config", path: "x/model.go", want: true},
		{name: "file name with extension", subject: "tune proc.go", path: "x/proc.go", want: true},
		{name: "word at the start", subject: "scope: narrow it", path: "x/scope.go", want: true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			row := commitRow{subject: c.subject, entries: []statusEntry{{status: "M", path: c.path}}}
			if got := turnSubject(row); got != c.want {
				t.Fatalf("turnSubject(%q, %s) = %v, want %v", c.subject, c.path, got, c.want)
			}
		})
	}
}
