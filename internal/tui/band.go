package tui

import (
	"cmp"
	"fmt"
	"math"
	"path/filepath"
	"regexp"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	kittyFontQuery  = "kitty-query-font_family"
	folderNameCells = 16
	moonSteps       = 14
)

var nerdFontName = regexp.MustCompile(`(?i:nerd ?font)|NF[MP]?(-|$)`)

var moonGlyphs = [moonSteps + 1]rune{0xE38D, 0xE38E, 0xE38F, 0xE390, 0xE391, 0xE392, 0xE393, 0xE394, 0xE395, 0xE396, 0xE397, 0xE398, 0xE399, 0xE39A, 0xE39B}

type segment struct {
	id, text string
	rank     int
}

type iconSet struct {
	nerd     bool
	effort   [4]string
	approval map[string]string
	sandbox  map[string]string
	folder   string
	queue    string
}

var unicodeIcons = iconSet{
	effort:   [4]string{"\u28c0", "\u28e4", "\u28f6", "\u28ff"},
	approval: map[string]string{"ask": "?", "auto": "\u2713", "yolo": "!"},
	sandbox:  map[string]string{"workspace-write": "\u22a1", "off": "\u2298"},
	folder:   "\u25b8",
	queue:    "+",
}

var nerdIcons = iconSet{
	nerd:     true,
	effort:   [4]string{"\U000f0873", "\U000f0875", "\U000f029a", "\U000f0874"},
	approval: map[string]string{"ask": "\U000f0e47", "auto": "\U000f167a", "yolo": "\U000f0238"},
	sandbox:  map[string]string{"workspace-write": "\U000f03d7", "off": "\U000f002a"},
	folder:   "\U000f0256",
	queue:    "\U000f051f",
}

func (m *model) iconSet() iconSet {
	switch m.screen.iconsSetting {
	case "nerd":
		return nerdIcons
	case "unicode":
		return unicodeIcons
	}
	if m.screen.kittyNerd {
		return nerdIcons
	}
	return unicodeIcons
}

func (m *model) bandRow() string {
	left, right := m.bandSegments()
	return m.screen.dockTheme().paint(roleDim, renderBand(&m.screen, left, right, m.screen.width))
}

func (m *model) bandSegments() (left, right []segment) {
	icons := m.iconSet()
	hidden := m.screen.statusHide
	add := func(list []segment, id, text string, rank int) []segment {
		if text == "" || slices.Contains(hidden, id) {
			return list
		}
		return append(list, segment{id: id, text: text, rank: rank})
	}
	left = []segment{{id: "model", text: cmp.Or(m.thread.modelName, "default")}}
	left = add(left, "effort", m.effortText(icons), 3)
	left = add(left, "approval", icons.approval[m.thread.approvalMode], 0)
	left = add(left, "sandbox", icons.sandbox[m.thread.sandbox], 0)
	left = add(left, "folder", m.folderText(icons), 1)
	left = add(left, "queue", queueText(icons, len(m.queue)), 2)
	right = add(nil, "context", m.contextText(icons), 0)
	right = add(right, "mark", m.conn.product, 4)
	return left, right
}

func (m *model) effortText(icons iconSet) string {
	levels := m.thread.levels[m.thread.modelName]
	rank := slices.Index(levels, m.thread.effort)
	if len(levels) < 2 || rank < 0 {
		return ""
	}
	return icons.effort[int(math.Round(float64(rank*3)/float64(len(levels)-1)))]
}

func (m *model) folderText(icons iconSet) string {
	if m.conn.opts.Cwd == "" {
		return ""
	}
	return icons.folder + " " + m.screen.fit(filepath.Base(m.conn.opts.Cwd), folderNameCells)
}

func queueText(icons iconSet, n int) string {
	if n == 0 {
		return ""
	}
	field := fmt.Sprintf("+%3d", n)
	if icons.nerd {
		return icons.queue + " " + field
	}
	return field
}

func (m *model) contextText(icons iconSet) string {
	fraction, ok := contextFraction(m.thread.usage)
	if !ok {
		return ""
	}
	field := fmt.Sprintf("%3d%%", int(math.Round(fraction*100)))
	if !icons.nerd {
		return field
	}
	return string(moonGlyphs[int(math.Round(fraction*moonSteps))]) + " " + field
}

func renderBand(s *screenState, left, right []segment, width int) string {
	room := terminalWidth(width)
	for bandCells(s, left, right) > room {
		var ok bool
		left, right, ok = dropLowest(left, right)
		if !ok {
			break
		}
	}
	if bandCells(s, left, right) > room {
		left = cutModel(s, left, right, room)
	}
	return s.fit(bandJoin(s, left, right, room), room)
}

func dropLowest(left, right []segment) ([]segment, []segment, bool) {
	lowest := 0
	for _, group := range [][]segment{left, right} {
		for _, seg := range group {
			if seg.rank > 0 && (lowest == 0 || seg.rank < lowest) {
				lowest = seg.rank
			}
		}
	}
	if lowest == 0 {
		return left, right, false
	}
	drop := func(seg segment) bool { return seg.rank == lowest }
	return slices.DeleteFunc(left, drop), slices.DeleteFunc(right, drop), true
}

func cutModel(s *screenState, left, right []segment, room int) []segment {
	i := slices.IndexFunc(left, func(seg segment) bool { return seg.id == "model" })
	if i < 0 {
		return left
	}
	other := bandCells(s, left, right) - s.cells(left[i].text)
	left[i].text = s.fit(left[i].text, max(0, room-other))
	return left
}

func bandCells(s *screenState, left, right []segment) int {
	total := groupCells(s, left)
	if len(right) > 0 {
		total += 1 + groupCells(s, right)
	}
	return total
}

func groupCells(s *screenState, group []segment) int {
	total := max(0, len(group)-1)
	for _, seg := range group {
		total += s.cells(seg.text)
	}
	return total
}

func bandJoin(s *screenState, left, right []segment, room int) string {
	row := joinSegments(left)
	if len(right) == 0 {
		return row
	}
	rightText := joinSegments(right)
	gap := max(1, room-s.cells(row)-s.cells(rightText))
	return row + strings.Repeat(" ", gap) + rightText
}

func joinSegments(group []segment) string {
	texts := make([]string, 0, len(group))
	for _, seg := range group {
		texts = append(texts, seg.text)
	}
	return strings.Join(texts, " ")
}

func (m *model) setStatusHide(value any) {
	items, _ := value.([]any)
	var hide []string
	for _, item := range items {
		id, ok := item.(string)
		if !ok {
			continue
		}
		if id == "model" {
			m.noticef("status_hide: model cannot be hidden")
			continue
		}
		hide = append(hide, id)
	}
	m.screen.statusHide = hide
}

func (m *model) applyEnvMsg(msg tea.EnvMsg) tea.Cmd {
	m.applyEnv(msg)
	m.screen.kittyWindow = msg.Getenv("KITTY_WINDOW_ID") != ""
	return m.requestKittyFont()
}

func (m *model) requestKittyFont() tea.Cmd {
	if !m.screen.kittyWindow || m.screen.iconsSetting != "auto" || m.screen.kittyAsked {
		return nil
	}
	m.screen.kittyAsked = true
	return tea.RequestCapability(kittyFontQuery)
}

func (m *model) applyCapability(msg tea.CapabilityMsg) {
	if value, ok := kittyFontValue(msg.Content); ok {
		m.screen.kittyNerd = nerdFontName.MatchString(value)
	}
}

func kittyFontValue(content string) (string, bool) {
	for field := range strings.SplitSeq(content, ";") {
		name, value, _ := strings.Cut(field, "=")
		if name == kittyFontQuery {
			return value, true
		}
	}
	return "", false
}

func (m *model) rememberLevels(models []rpc.Model) {
	if m.thread.levels == nil {
		m.thread.levels = make(map[string][]string)
	}
	for _, item := range models {
		levels := make([]string, 0, len(item.SupportedReasoningEfforts))
		for _, option := range item.SupportedReasoningEfforts {
			levels = append(levels, option.ReasoningEffort)
		}
		m.thread.levels[item.ID] = levels
	}
}
