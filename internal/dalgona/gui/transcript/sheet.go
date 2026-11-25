package transcript

import (
	"cmp"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type fieldText struct {
	text    string
	ready   bool
	absent  bool
	large   bool
	total   uint64
	lines   int
	counted bool
	capped  bool
	failed  *body
	content *body
}

type reader struct {
	loader *loader
	thread state.ThreadKey
	item   state.ItemView
}

func (r reader) text(field string) fieldText {
	return r.load(field, false)
}

func (r reader) fullText(field string) fieldText {
	return r.load(field, true)
}

func (r reader) load(field string, full bool) fieldText {
	tail := ""
	if r.item.Field == field {
		tail = string(r.item.Tail)
	}
	b, ok := r.loader.want(r.thread, r.item, field, full)
	if !ok {
		return fieldText{text: tail, ready: true, absent: tail == "" && r.item.Field != field}
	}
	ft := fieldText{text: b.text() + tail, ready: b.complete(), large: b.large(), total: b.total, content: b}
	if b.err != nil {
		ft.failed = b
	}
	if b.large() {
		r.count(&ft, b.ref)
	}
	return ft
}

func (r reader) count(ft *fieldText, ref rpc.ContentReference) {
	c := r.loader.counter(r.thread, r.item.Key, ref)
	ft.lines, ft.counted, ft.capped = c.lineTotal(), c.complete(), c.total > countLimit
	if c.err != nil && ft.failed == nil {
		ft.failed = c
	}
}

func (r reader) output(field string) fieldText {
	if _, ok := refNamed(r.item.Header, fieldSpill); ok {
		return r.text(fieldSpill)
	}
	return r.text(field)
}

type attachment struct {
	Kind  string
	Mime  string
	Bytes int64
	Path  string
	Name  string
	Alt   string
	Index int
}

type sheet struct {
	format          document.Format
	source          string
	open            bool
	roles           []lineRole
	note            string
	full            bool
	wait            bool
	failed          *body
	projectionErr   error
	images          []attachment
	keywords        []keyword
	advisor         string
	severity        string
	files           int
	args            bool
	sources         []*body
	diffLines       []diffLine
	diffPaths       []string
	diffFiles       []int
	diffHunks       []int
	diffLimited     bool
	patchProjection bool
}

func (s sheet) incomplete() bool {
	return s.wait || s.failed != nil || s.projectionErr != nil
}

func (s *sheet) need(ft fieldText) {
	if ft.content != nil {
		s.sources = append(s.sources, ft.content)
	}
	if ft.failed != nil {
		s.failed = ft.failed
	}
	if !ft.ready && ft.failed == nil {
		s.wait = true
	}
}

type sheetInput struct {
	entry     Entry
	pres      Presentation
	read      reader
	expanded  bool
	diffSplit bool
	diffFull  bool
}

func buildSheet(in sheetInput) sheet {
	switch in.entry.Kind {
	case KindUser:
		return userSheet(in)
	case KindAssistant:
		ft := in.read.fullText("text")
		s := sheet{format: document.FormatMarkdown, source: ft.text, open: in.entry.Item.Provisional}
		s.need(ft)
		return s
	case KindThinking:
		return thinkingSheet(in)
	case KindCommand:
		return commandSheet(in)
	case KindFileChange:
		return changeSheet(in)
	case KindTool:
		return toolSheet(in)
	case KindAdvisor:
		return advisorSheet(in)
	case KindPlanItem, KindCompaction, KindOther, KindGroup:
	}
	return plainSheet(in)
}

func plainSheet(in sheetInput) sheet {
	switch in.entry.Kind {
	case KindPlanItem:
		ft := in.read.text("text")
		s := sheet{format: document.FormatNotice, source: ft.text}
		s.need(ft)
		return s
	case KindCompaction:
		return sheet{format: document.FormatNotice, source: "Context compacted · earlier turns remain in the journal"}
	case KindUser, KindAssistant, KindThinking, KindCommand, KindFileChange, KindTool, KindAdvisor, KindOther, KindGroup:
	}
	return sheet{format: document.FormatNotice, source: kindLabel(in.entry.Item.Header.Type), full: true}
}

type userPart struct {
	Type    string `json:"type"`
	Mime    string `json:"mime"`
	Bytes   int64  `json:"bytes"`
	Path    string `json:"path"`
	Name    string `json:"name"`
	Alt     string `json:"alt"`
	AltText string `json:"altText"`
}

func userSheet(in sheetInput) sheet {
	ft := in.read.text("text")
	s := sheet{format: document.FormatNotice, source: ft.text, images: userImages(in.entry.Item.Header)}
	s.need(ft)
	if ft.ready {
		s.keywords = validKeywords(s.source, in.entry.Facts.keywords)
	}
	return s
}

func userImages(h rpc.ItemHeader) []attachment {
	var parts []userPart
	if raw, ok := h.Metadata["content"]; !ok || json.Unmarshal(raw, &parts) != nil {
		return nil
	}
	out := make([]attachment, 0, len(parts))
	for _, p := range parts {
		if p.Type != "text" {
			out = append(out, attachment{Kind: p.Type, Mime: p.Mime, Bytes: p.Bytes, Path: p.Path, Name: p.Name, Alt: cmp.Or(p.Alt, p.AltText), Index: len(out)})
		}
	}
	return out
}

func validKeywords(text string, matches []keyword) []keyword {
	out := make([]keyword, 0, len(matches))
	for _, m := range matches {
		if m.Start < 0 || m.End > len(text) || m.Start >= m.End || !boundary(text, m.Start) || !boundary(text, m.End) {
			continue
		}
		out = append(out, m)
	}
	return out
}

func boundary(text string, at int) bool {
	return at == len(text) || text[at]&0xC0 != 0x80
}

func thinkingSheet(in sheetInput) sheet {
	ft := in.read.text("content")
	text := ft.text
	if ft.ready && text == "" {
		text = in.entry.Facts.summary
	}
	s := sheet{format: document.FormatNotice}
	s.need(ft)
	switch in.pres.Mode {
	case ModeLine:
		s.source = "Thinking · " + countLabel(lineCount(text), "line", "lines") + " · ctrl+o"
		s.full = true
	case ModeTail:
		s.source, _ = lastLines(text, 1)
		s.full = true
	case ModeFull, ModeHidden:
		s.open = in.entry.Live()
		if in.expanded {
			s.source = text
		}
	}
	return s
}

func tailSheet(s *sheet, text string, pres Presentation, ft fieldText) {
	s.source = text
	total := 0
	if pres.Mode == ModeTail {
		cut, n := lastLines(text, pres.Tail)
		total = n
		if !promotedComplete(ft) {
			s.source = cut
		}
	}
	switch {
	case ft.large:
		s.note, s.full = largeNote(pres, ft), true
	case total > pres.Tail:
		s.note, s.full = "Showing the last "+strconv.Itoa(pres.Tail)+" of "+strconv.Itoa(total)+" lines.", true
	}
}

func promotedComplete(ft fieldText) bool {
	b := ft.content
	return b != nil && b.full && b.complete()
}

func largeNote(pres Presentation, ft fieldText) string {
	size := byteSize(ft.total)
	switch {
	case !ft.counted:
		return "Showing the end of " + size + " of output; counting its lines."
	case pres.Mode == ModeTail:
		return "Showing the last " + strconv.Itoa(pres.Tail) + " of " + countWord(ft) + " lines."
	}
	return "Showing the end of " + countWord(ft) + " lines (" + size + ") of output; open the full text for all of it."
}

func countWord(ft fieldText) string {
	n := strconv.Itoa(ft.lines)
	if ft.capped {
		return "more than " + n
	}
	return n
}

func commandSheet(in sheetInput) sheet {
	s := sheet{format: document.FormatCode}
	out := in.read.output("aggregatedOutput")
	s.need(out)
	tailSheet(&s, out.text, in.pres, out)
	s.full = s.full || in.pres.Mode != ModeFull
	return s
}

func byteSize(n uint64) string {
	switch {
	case n >= 1<<20:
		return strconv.FormatFloat(float64(n)/(1<<20), 'f', 1, 64) + " MiB"
	case n >= 1<<10:
		return strconv.FormatFloat(float64(n)/(1<<10), 'f', 1, 64) + " KiB"
	}
	return strconv.FormatUint(n, 10) + " bytes"
}

func toolSheet(in sheetInput) sheet {
	s := sheet{format: document.FormatCode, full: in.pres.Mode != ModeFull, args: in.pres.Mode == ModeFull}
	args := in.read.text("arguments")
	if in.pres.Mode == ModeLine {
		s.need(args)
		s.format = document.FormatNotice
		s.source = "Eval · " + countLabel(lineCount(evalCode(args.text)), "line", "lines") + " of code · " + stateWord(in.entry.status())
		return s
	}
	out := toolOutput(in)
	s.need(out)
	tailSheet(&s, tailored(out.text), in.pres, out)
	if s.args && in.expanded {
		s.need(args)
		s.projectArguments(args.text, in.read.loader)
	}
	return s
}

func (s *sheet) projectArguments(raw string, l *loader) {
	prefix, err := prettyJSON(raw, len(s.source), l)
	if err != nil {
		s.projectionErr, s.full = err, true
		return
	}
	s.roles = argumentRoles(prefix)
	s.source = prefix + "\n" + s.source
}

func completeField(ft fieldText) bool {
	return ft.ready && (ft.content == nil || ft.content.start == 0)
}

func argumentRoles(text string) []lineRole {
	roles := make([]lineRole, strings.Count(text, "\n")+1)
	for i := range roles {
		roles[i] = roleDim
	}
	return roles
}

func toolOutput(in sheetInput) fieldText {
	if _, ok := refNamed(in.entry.Item.Header, fieldSpill); ok {
		return in.read.text(fieldSpill)
	}
	switch in.entry.Item.Header.Type {
	case "mcpToolCall":
		res := in.read.text("result")
		if res.absent && in.entry.Facts.errText != "" {
			return fieldText{text: in.entry.Facts.errText, ready: true}
		}
		return partsText(res, "content")
	case "dynamicToolCall":
		return partsText(in.read.text("contentItems"), "")
	}
	return in.read.text("output")
}

type textPart struct {
	Text string `json:"text"`
}

func partsText(ft fieldText, member string) fieldText {
	if !ft.ready || ft.text == "" {
		return ft
	}
	raw := []byte(ft.text)
	if member != "" {
		var wrapper map[string]jsontext.Value
		if json.Unmarshal(raw, &wrapper) != nil {
			return ft
		}
		raw = wrapper[member]
	}
	var parts []textPart
	if json.Unmarshal(raw, &parts) != nil {
		return ft
	}
	texts := make([]string, 0, len(parts))
	for _, p := range parts {
		texts = append(texts, p.Text)
	}
	ft.text = strings.Join(texts, "\n")
	return ft
}

func evalCode(args string) string {
	var call struct {
		Code string `json:"code"`
	}
	if json.Unmarshal([]byte(args), &call) != nil {
		return ""
	}
	return call.Code
}

func advisorSheet(in sheetInput) sheet {
	args := in.read.text("arguments")
	note := partsText(in.read.text("contentItems"), "")
	s := sheet{format: document.FormatNotice, source: note.text, advisor: toolAdvisor}
	s.need(args)
	s.need(note)
	var call struct {
		Advisor  string `json:"advisor"`
		Severity string `json:"severity"`
	}
	if args.ready && json.Unmarshal([]byte(args.text), &call) == nil {
		s.advisor, s.severity = cmpOr(call.Advisor, toolAdvisor), call.Severity
	}
	return s
}

func cmpOr(a, b string) string {
	if a != "" {
		return a
	}
	return b
}

func advisorLabel(name, severity string) string {
	label := "advisor"
	if name != toolAdvisor {
		label += " " + name
	}
	return label + " · " + severity + ":"
}

func changeSheet(in sheetInput) sheet {
	s := sheet{format: document.FormatPatch, full: in.pres.Mode != ModeFull}
	changes, ft, err := readChanges(in.read)
	s.need(ft)
	if err != nil {
		s.source = "The file changes could not be read: " + err.Error()
		return s
	}
	if !completeField(ft) {
		return s
	}
	return diffSheet(s, changes, in)
}

func diffSheet(s sheet, changes []change, in sheetInput) sheet {
	view := diffView{tail: in.entry.Failed(), split: in.diffSplit}
	if in.pres.Mode == ModeTail && !in.diffFull && !in.diffSplit {
		view.limit = in.pres.Tail
	}
	p := presentChanges(changes, view)
	s.source, s.roles = p.source()
	s.patchProjection = true
	s.diffLines = p.lines
	s.diffLimited = p.limited
	for _, c := range changes {
		s.diffPaths = append(s.diffPaths, c.Path)
	}
	for i := range s.diffLines {
		d := &s.diffLines[i]
		if d.role == roleFile {
			s.diffFiles = append(s.diffFiles, i)
		}
		if d.role == roleHunk {
			s.diffHunks = append(s.diffHunks, i)
		}
		d.text = ""
	}
	s.files = len(changes)
	return s
}

func readChanges(r reader) ([]change, fieldText, error) {
	if len(r.item.Changes) > 0 {
		return liveChanges(r.item.Changes), fieldText{ready: true}, nil
	}
	ft := r.fullText("changes")
	if !completeField(ft) || ft.absent {
		return nil, ft, nil
	}
	changes, err := parseChanges([]byte(ft.text))
	return changes, ft, err
}

func fileCount(in sheetInput) int {
	if len(in.entry.Item.Changes) > 0 {
		return len(in.entry.Item.Changes)
	}
	ft := in.read.fullText("changes")
	if !ft.ready {
		return 1
	}
	parsed, err := parseChanges([]byte(ft.text))
	if err != nil {
		return 1
	}
	return len(parsed)
}
