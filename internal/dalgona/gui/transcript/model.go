package transcript

import (
	json "encoding/json/v2"
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"unicode"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type Kind uint8

const (
	KindUser Kind = iota + 1
	KindAssistant
	KindThinking
	KindCommand
	KindFileChange
	KindTool
	KindAdvisor
	KindPlanItem
	KindCompaction
	KindOther
	KindGroup
)

type Level uint8

const (
	LevelLow Level = iota + 1
	LevelMedium
	LevelHigh
)

func ParseLevel(setting string) Level {
	switch setting {
	case "low":
		return LevelLow
	case "high":
		return LevelHigh
	}
	return LevelMedium
}

const (
	itemPlan           = "plan"
	itemFunctionOutput = "functionCallOutput"
	toolAdvisor        = "advisor"
	toolEval           = "eval"
	statusAwaiting     = "awaitingApproval"
	statusTimedOut     = "timedOut"
	tailLines          = 10
	diffLimit          = 60
)

var itemKinds = map[string]Kind{
	rpc.KindUser:       KindUser,
	rpc.KindAgent:      KindAssistant,
	rpc.KindReasoning:  KindThinking,
	rpc.KindCommand:    KindCommand,
	rpc.KindFileChange: KindFileChange,
	rpc.KindMCP:        KindTool,
	rpc.KindDynamic:    KindTool,
	itemFunctionOutput: KindTool,
	itemPlan:           KindPlanItem,
	rpc.KindCompaction: KindCompaction,
}

type keyword struct {
	Word       string
	Start, End int
}

type facts struct {
	kind     Kind
	status   string
	exit     *int64
	duration *int64
	tool     string
	errText  string
	summary  string
	keywords []keyword
}

func readFacts(h rpc.ItemHeader) facts {
	f := facts{status: metaText(h, "status"), tool: metaText(h, "tool"), exit: metaInt(h, "exitCode"), duration: metaInt(h, "durationMs")}
	if f.tool == "" {
		f.tool = metaText(h, "name")
	}
	var failure struct {
		Message string `json:"message"`
	}
	if raw, ok := h.Metadata["error"]; ok && json.Unmarshal(raw, &failure) == nil {
		f.errText = failure.Message
	}
	var summary []string
	if raw, ok := h.Metadata["summary"]; ok && json.Unmarshal(raw, &summary) == nil {
		f.summary = strings.Join(summary, "\n")
	}
	f.keywords = metaKeywords(h)
	f.kind = kindOf(h.Type, f.tool)
	return f
}

func kindOf(itemType, tool string) Kind {
	if itemType == rpc.KindDynamic && tool == toolAdvisor {
		return KindAdvisor
	}
	if k, ok := itemKinds[itemType]; ok {
		return k
	}
	return KindOther
}

func metaText(h rpc.ItemHeader, name string) string {
	var s string
	if raw, ok := h.Metadata[name]; ok && json.Unmarshal(raw, &s) == nil {
		return s
	}
	return ""
}

func metaInt(h rpc.ItemHeader, name string) *int64 {
	var n int64
	if raw, ok := h.Metadata[name]; ok && json.Unmarshal(raw, &n) == nil {
		return &n
	}
	return nil
}

func metaKeywords(h rpc.ItemHeader) []keyword {
	raw, ok := h.Metadata["keywords"]
	if !ok {
		return nil
	}
	var matches []struct {
		Word      string `json:"word"`
		ByteRange struct {
			Start int `json:"start"`
			End   int `json:"end"`
		} `json:"byteRange"`
	}
	if json.Unmarshal(raw, &matches) != nil {
		return nil
	}
	out := make([]keyword, 0, len(matches))
	for _, m := range matches {
		out = append(out, keyword{Word: m.Word, Start: m.ByteRange.Start, End: m.ByteRange.End})
	}
	return out
}

type Entry struct {
	Thread  state.ThreadKey
	Turn    string
	Item    state.ItemView
	Facts   facts
	Kind    Kind
	Nested  bool
	Members []Entry
}

func toolKind(k Kind) bool {
	return k == KindCommand || k == KindFileChange || k == KindTool
}

func (e Entry) status() string {
	return e.Facts.status
}

func (e Entry) Failed() bool {
	return toolKind(e.Kind) && e.Facts.status == rpc.StatusFailed
}

func (e Entry) Live() bool {
	switch e.Kind {
	case KindAssistant, KindThinking:
		return e.Item.Provisional
	case KindCommand, KindFileChange, KindTool, KindAdvisor:
		return e.Facts.status == rpc.StatusInProgress
	case KindGroup:
		for _, m := range e.Members {
			if m.Live() {
				return true
			}
		}
	case KindUser, KindPlanItem, KindCompaction, KindOther:
	}
	return false
}

func orderTurn(thread state.ThreadKey, turn state.TurnView, factsOf func(state.ItemView) facts) []Entry {
	present := make(map[string]bool, len(turn.Items))
	for _, it := range turn.Items {
		present[it.Key.Item] = true
	}
	children := map[string][]int{}
	roots := make([]int, 0, len(turn.Items))
	for i, it := range turn.Items {
		if owner, ok := nestedOwner(it.Key.Item); ok && present[owner] {
			children[owner] = append(children[owner], i)
			continue
		}
		roots = append(roots, i)
	}
	out := make([]Entry, 0, len(turn.Items))
	var emit func(i int, nested bool)
	emit = func(i int, nested bool) {
		it := turn.Items[i]
		f := factsOf(it)
		out = append(out, Entry{Thread: thread, Turn: turn.ID, Item: it, Facts: f, Kind: f.kind, Nested: nested})
		for _, c := range children[it.Key.Item] {
			emit(c, true)
		}
	}
	for _, i := range roots {
		emit(i, false)
	}
	return out
}

func nestedOwner(id string) (string, bool) {
	at := strings.LastIndexByte(id, '/')
	if at <= 0 || at == len(id)-1 {
		return "", false
	}
	if _, err := strconv.ParseUint(id[at+1:], 10, 32); err != nil {
		return "", false
	}
	return id[:at], true
}

func groupTools(entries []Entry) []Entry {
	out := make([]Entry, 0, len(entries))
	var group []Entry
	flush := func() {
		if len(group) > 0 {
			first := group[0]
			out = append(out, Entry{Thread: first.Thread, Turn: first.Turn, Item: first.Item, Kind: KindGroup, Members: group})
		}
		group = nil
	}
	for _, e := range entries {
		if len(group) > 0 && group[0].Turn != e.Turn {
			flush()
		}
		if toolKind(e.Kind) && !e.Failed() {
			group = append(group, e)
			continue
		}
		flush()
		out = append(out, e)
	}
	flush()
	return out
}

type Mode uint8

const (
	ModeFull Mode = iota + 1
	ModeLine
	ModeTail
	ModeHidden
)

type Presentation struct {
	Mode Mode
	Tail int
}

func present(e Entry, level Level) Presentation {
	switch {
	case e.Failed():
		return Presentation{Mode: ModeTail, Tail: tailLines}
	case level == LevelHigh:
		return Presentation{Mode: ModeFull}
	case e.Kind == KindThinking && e.Live():
		return Presentation{Mode: ModeTail, Tail: 1}
	case e.Kind == KindThinking:
		return Presentation{Mode: ModeLine}
	case !toolKind(e.Kind):
		return Presentation{Mode: ModeFull}
	case level == LevelLow:
		return Presentation{Mode: ModeHidden}
	}
	return mediumTool(e)
}

func mediumTool(e Entry) Presentation {
	switch {
	case e.Kind == KindCommand:
		return Presentation{Mode: ModeTail, Tail: tailLines}
	case e.Kind == KindFileChange:
		return Presentation{Mode: ModeTail, Tail: diffLimit}
	case e.Facts.tool == toolEval:
		return Presentation{Mode: ModeLine}
	}
	return Presentation{Mode: ModeFull}
}

var stateWords = map[string]string{
	"active":           "Working",
	"inProgress":       "Working",
	statusAwaiting:     "Waiting",
	"completed":        "Done",
	"failed":           "Failed",
	"declined":         "Declined",
	"interrupted":      "Stopped",
	"timedOut":         "Timed out",
	"idle":             "Ready",
	"notLoaded":        "Ready",
	"systemError":      "Failed",
	"disconnected":     "Offline",
	"waitingOnRequest": "Waiting",
}

func stateWord(status string) string {
	if w, ok := stateWords[status]; ok {
		return w
	}
	return status
}

var planWords = map[string]string{"completed": "Done", "inProgress": "Working", "pending": "Pending"}

func planWord(status string) string {
	if w, ok := planWords[status]; ok {
		return w
	}
	return status
}

func formatDuration(ms int64) string {
	switch {
	case ms < 1000:
		return fmt.Sprintf("%d ms", ms)
	case ms < 60_000:
		return fmt.Sprintf("%ds", ms/1000)
	case ms < 3_600_000:
		return fmt.Sprintf("%dm %02ds", ms/60_000, ms/1000%60)
	}
	return fmt.Sprintf("%dh %02dm", ms/3_600_000, ms/60_000%60)
}

func countLabel(n int, one, many string) string {
	if n == 1 {
		return "1 " + one
	}
	return strconv.Itoa(n) + " " + many
}

func lineCount(text string) int {
	trimmed := strings.TrimRight(text, "\n")
	if trimmed == "" {
		return 0
	}
	return strings.Count(trimmed, "\n") + 1
}

func lastLines(text string, n int) (string, int) {
	text = strings.TrimSuffix(text, "\n")
	total := strings.Count(text, "\n") + 1
	if total <= n {
		return text, total
	}
	cut := len(text)
	for range n {
		cut = strings.LastIndexByte(text[:cut], '\n')
	}
	return text[cut+1:], total
}

var anchorLine = regexp.MustCompile(`^\[.*\S(?:#[0-9A-F]{4}(?:[0-9A-F]{4})?|@r[0-9a-f]{32}\.[0-9a-z]+)\]$`)

func tailored(text string) string {
	if !strings.Contains(text, "[") {
		return text
	}
	lines := strings.Split(text, "\n")
	kept := lines[:0]
	for _, line := range lines {
		if !anchorLine.MatchString(line) {
			kept = append(kept, line)
		}
	}
	return strings.Join(kept, "\n")
}

func kindLabel(kind string) string {
	var b strings.Builder
	for i, r := range kind {
		if i > 0 && unicode.IsUpper(r) {
			b.WriteByte(' ')
		}
		b.WriteRune(unicode.ToLower(r))
	}
	words := b.String()
	if words == "" {
		return "Unknown item"
	}
	first := []rune(words)
	first[0] = unicode.ToUpper(first[0])
	return string(first)
}

func groupSummary(members []Entry, changes func(Entry) int) string {
	commands, edits, calls := 0, 0, 0
	for _, m := range members {
		switch m.Kind {
		case KindCommand:
			commands++
		case KindFileChange:
			edits += max(changes(m), 1)
		case KindUser, KindAssistant, KindThinking, KindTool, KindAdvisor, KindPlanItem, KindCompaction, KindOther, KindGroup:
			calls++
		}
	}
	var parts []string
	if commands > 0 {
		parts = append(parts, "Ran "+countLabel(commands, "command", "commands"))
	}
	if edits > 0 {
		parts = append(parts, "edited "+countLabel(edits, "file", "files"))
	}
	if calls > 0 {
		parts = append(parts, "called "+countLabel(calls, "tool", "tools"))
	}
	return strings.Join(parts, " · ")
}
