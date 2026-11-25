package transcript

import (
	json "encoding/json/v2"
	"errors"
	"math"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type lineRole uint8

const (
	roleText lineRole = iota
	roleAdd
	roleDelete
	roleHunk
	roleMeta
	roleNote
	roleFile
	roleOmitted
	roleStrong
	roleDim
	roleFailed
	roleWarning
	roleCurrent
)

type change struct {
	Path        string
	Kind        string
	Diff        string
	Omitted     bool
	Additions   int
	Deletions   int
	Counted     bool
	DeleteCount bool
}

type wireChange struct {
	Path string `json:"path"`
	Kind struct {
		Type string `json:"type"`
	} `json:"kind"`
	Diff        string `json:"diff"`
	DiffOmitted bool   `json:"diffOmitted"`
	Additions   *int   `json:"additions"`
	Deletions   *int   `json:"deletions"`
}

func parseChanges(raw []byte) ([]change, error) {
	var wire []wireChange
	if err := json.Unmarshal(raw, &wire); err != nil {
		return nil, err
	}
	out := make([]change, 0, len(wire))
	for _, w := range wire {
		c := change{Path: w.Path, Kind: w.Kind.Type, Diff: w.Diff, Omitted: w.DiffOmitted}
		if w.Additions != nil {
			c.Additions, c.Counted = *w.Additions, true
		}
		if w.Deletions != nil {
			c.Deletions, c.DeleteCount = *w.Deletions, true
		}
		out = append(out, c)
	}
	return out, nil
}

func liveChanges(changes []rpc.FileChange) []change {
	out := make([]change, 0, len(changes))
	for _, c := range changes {
		counted := !c.DiffOmitted
		out = append(out, change{Path: c.Path, Kind: c.Kind, Diff: c.Diff, Omitted: c.DiffOmitted, Additions: c.Additions, Deletions: c.Deletions, Counted: counted, DeleteCount: counted})
	}
	return out
}

var errPatchOmitted = errors.New("the daemon did not retain the complete patch")

var changeWords = map[string]string{rpc.KindAdd: "Added", rpc.KindUpdate: "Changed", rpc.KindDelete: "Deleted"}

func changeWord(kind string) string {
	if w, ok := changeWords[kind]; ok {
		return w
	}
	return kind
}

func (c change) heading() string {
	parts := []string{c.Path, changeWord(c.Kind)}
	if c.Counted {
		counts := "+" + strconv.Itoa(c.Additions)
		if c.DeleteCount {
			counts += " −" + strconv.Itoa(c.Deletions)
		}
		parts = append(parts, counts)
	}
	return strings.Join(parts, " · ")
}

type diffLine struct {
	role    lineRole
	text    string
	old     int
	new     int
	file    int
	hunk    int
	start   int
	cut     int
	pair    bool
	context bool
}

type diffCursor struct {
	old, fresh, oldLeft, freshLeft int
}

func diffLines(diff string) []diffLine {
	if diff == "" {
		return nil
	}
	var out []diffLine
	var cursor diffCursor
	hunk := 0
	for raw := range strings.SplitSeq(strings.TrimSuffix(diff, "\n"), "\n") {
		line := strings.TrimSuffix(raw, "\r")
		d := diffLine{text: line, role: diffRole(line, cursor.oldLeft > 0 || cursor.freshLeft > 0)}
		if d.role == roleHunk {
			cursor = hunkRange(line)
			hunk++
		}
		d.hunk = hunk
		cursor.number(&d)
		out = append(out, d)
	}
	return out
}

func (c *diffCursor) number(d *diffLine) {
	switch {
	case d.role == roleAdd && c.freshLeft > 0:
		d.new = c.fresh
		c.fresh++
		c.freshLeft--
	case d.role == roleDelete && c.oldLeft > 0:
		d.old = c.old
		c.old++
		c.oldLeft--
	case strings.HasPrefix(d.text, " ") && c.oldLeft > 0 && c.freshLeft > 0:
		d.old, d.new = c.old, c.fresh
		c.old++
		c.fresh++
		c.oldLeft--
		c.freshLeft--
	}
}

func hunkRange(line string) (c diffCursor) {
	fields, _, ok := strings.Cut(strings.TrimPrefix(line, "@@ -"), " @@")
	if !ok {
		return c
	}
	a, b, ok := strings.Cut(fields, " +")
	if !ok {
		return c
	}
	c.old, c.oldLeft = patchRange(a)
	c.fresh, c.freshLeft = patchRange(b)
	return c
}

func patchRange(text string) (start, count int) {
	first, length, explicit := strings.Cut(text, ",")
	start, err := strconv.Atoi(first)
	if err != nil || start < 0 {
		return 0, 0
	}
	if !explicit && start != math.MaxInt {
		return start, 1
	}
	count, err = strconv.Atoi(length)
	if err != nil || count < 0 || count > math.MaxInt-start {
		return 0, 0
	}
	return start, count
}

func splitDiffLines(lines []diffLine) []diffLine {
	out := make([]diffLine, 0, len(lines))
	for len(lines) > 0 {
		run := takePatchEdits(lines)
		j := len(run.before) + len(run.after) + len(run.notes)
		if j == 0 {
			d := lines[0]
			d.pair = d.old > 0 && d.new > 0
			d.context = d.pair
			out = append(out, d)
			lines = lines[1:]
			continue
		}
		for i := range max(len(run.before), len(run.after)) {
			out = append(out, pairedLine(run.before, run.after, i))
		}
		out = append(out, run.notes...)
		lines = lines[j:]
	}
	return out
}

type patchEdits struct {
	before, after, notes []diffLine
}

func takePatchEdits(lines []diffLine) (run patchEdits) {
	for i, d := range lines {
		switch {
		case d.role == roleDelete:
			run.before = append(run.before, d)
		case d.role == roleAdd:
			run.after = append(run.after, d)
		case i != 0 && strings.HasPrefix(d.text, "\\"):
			run.notes = append(run.notes, d)
		default:
			return run
		}
	}
	return run
}

func pairedLine(before, after []diffLine, i int) diffLine {
	if i >= len(before) {
		d := after[i]
		d.pair = true
		return d
	}
	d := before[i]
	d.pair, d.cut = true, len(d.text)
	if i < len(after) {
		d.text += "\t" + after[i].text
		d.new = after[i].new
	}
	return d
}

func diffRole(line string, inHunk bool) lineRole {
	if strings.HasPrefix(line, "@@ ") {
		return roleHunk
	}
	if inHunk {
		switch {
		case strings.HasPrefix(line, "+"):
			return roleAdd
		case strings.HasPrefix(line, "-"):
			return roleDelete
		}
		return roleText
	}
	for _, prefix := range [...]string{"--- ", "+++ ", "Binary files ", "GIT binary patch", "rename ", "Content unavailable ", "missing blob"} {
		if strings.HasPrefix(line, prefix) {
			return roleMeta
		}
	}
	return roleText
}

type diffView struct {
	limit int
	tail  bool
	split bool
}

type presented struct {
	lines   []diffLine
	limited bool
}

func (p *presented) add(role lineRole, text string) {
	p.lines = append(p.lines, diffLine{role: role, text: text})
}

func presentChanges(changes []change, v diffView) presented {
	var p presented
	for i, c := range changes {
		start := len(p.lines)
		p.addChange(c, v)
		for j := start; j < len(p.lines); j++ {
			p.lines[j].file = i
		}
	}
	return p
}

func (p *presented) addChange(c change, v diffView) {
	p.add(roleFile, "diff "+c.heading())
	switch {
	case c.Omitted:
		p.add(roleOmitted, "Diff not shown.")
	case c.Diff == "":
		p.add(roleNote, "No line changes reported.")
	default:
		lines := diffLines(c.Diff)
		for k := range lines {
			if strings.HasPrefix(lines[k].text, "diff ") {
				lines[k].text = " " + lines[k].text
			}
		}
		if v.split {
			lines = splitDiffLines(lines)
		}
		p.addFile(lines, v)
	}
}

func (p *presented) addFile(lines []diffLine, v diffView) {
	kept := lines
	hidden := 0
	if v.limit > 0 && len(lines) > v.limit {
		hidden = len(lines) - v.limit
		p.limited = true
		kept = lines[:v.limit]
		if v.tail {
			kept = lines[hidden:]
		}
	}
	note := "… " + strconv.Itoa(hidden) + " more " + plural(hidden, "line", "lines") + " omitted; open the full text to see them"
	if hidden > 0 && v.tail {
		p.add(roleNote, note)
	}
	p.lines = append(p.lines, kept...)
	if hidden > 0 && !v.tail {
		p.add(roleNote, note)
	}
}

func plural(n int, one, many string) string {
	if n == 1 {
		return one
	}
	return many
}

func (p presented) source() (string, []lineRole) {
	var b strings.Builder
	roles := make([]lineRole, 0, len(p.lines))
	for i := range p.lines {
		if i > 0 {
			b.WriteByte('\n')
		}
		p.lines[i].start = b.Len()
		line := p.lines[i]
		b.WriteString(line.text)
		roles = append(roles, line.role)
	}
	return b.String(), roles
}
