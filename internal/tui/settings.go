package tui

import (
	"cmp"
	"fmt"
	"maps"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	settingsHint = "↑/↓ row · ←/→ value · Space toggle · Enter save · Esc close"
	detailRows   = 7
)

type settingRow struct {
	key     string
	label   string
	options []string
}

var settingRows = []settingRow{
	{key: "approval", label: "Approval", options: []string{"ask", "auto", "yolo"}},
	{key: "verbosity", label: "Verbosity", options: []string{"low", "medium", "high"}},
	{key: "theme", label: "Theme", options: []string{"auto", "auto-soft", "day", "soft-day", "night", "soft-night"}},
	{key: "icons", label: "Icons", options: []string{"auto", "unicode", "nerd"}},
	{key: "statusHide", label: "Status"},
	{key: "editReview", label: "Edit review", options: []string{"off", "on"}},
	{key: "notifications", label: "Notifications", options: []string{"on", "off"}},
}

var statusItemIDs = []string{"effort", "approval", "sandbox", "folder", "queue", "context", "mark"}

type settingsPanel struct {
	rows       []settingRow
	focus      int
	statusItem int
	saved      map[string]string
	preview    map[string]string
	saveErr    string
}

type settingEdit struct {
	KeyPath       string `json:"keyPath"`
	Value         any    `json:"value"`
	MergeStrategy string `json:"mergeStrategy"`
}

type settingsBatch struct {
	Edits []settingEdit `json:"edits"`
}

func (p *settingsPanel) clone() *settingsPanel {
	return &settingsPanel{rows: p.rows, saved: maps.Clone(p.saved), preview: maps.Clone(p.preview)}
}

func (p *settingsPanel) edits() []settingEdit {
	var edits []settingEdit
	for _, row := range p.rows {
		if p.preview[row.key] == p.saved[row.key] {
			continue
		}
		edits = append(edits, settingEdit{KeyPath: row.key, Value: wireSettingValue(row.key, p.preview[row.key]), MergeStrategy: "replace"})
	}
	return edits
}

func (m *model) setPref(key, text string) {
	if m.prefs == nil {
		m.prefs = map[string]string{}
	}
	m.prefs[key] = text
}

func (m *model) rememberSetting(setting settingValue) {
	switch setting.Key {
	case "approval":
		if text, ok := setting.Value.(string); ok {
			m.setPref("approval", text)
		}
	case "editReview", "notifications":
		m.setPref(setting.Key, flagText(setting.Value))
	}
}

func flagText(value any) string {
	flag, ok := value.(bool)
	if !ok {
		return ""
	}
	if flag {
		return "on"
	}
	return "off"
}

func hiddenIDs(text string) []string {
	if text == "" {
		return []string{}
	}
	return strings.Split(text, ",")
}

func hiddenText(ids []string) string {
	var hidden []string
	for _, id := range statusItemIDs {
		if slices.Contains(ids, id) {
			hidden = append(hidden, id)
		}
	}
	return strings.Join(hidden, ",")
}

func wireSettingValue(key, text string) any {
	switch key {
	case "statusHide":
		return hiddenIDs(text)
	case "editReview", "notifications":
		return text == "on"
	}
	return text
}

func (m *model) savedSettings() map[string]string {
	return map[string]string{
		"approval":      cmp.Or(m.prefs["approval"], "auto"),
		"verbosity":     cmp.Or(m.screen.verbosity, "medium"),
		"theme":         cmp.Or(m.screen.themeSetting, "auto"),
		"icons":         cmp.Or(m.screen.iconsSetting, "auto"),
		"statusHide":    hiddenText(m.screen.statusHide),
		"editReview":    cmp.Or(m.prefs["editReview"], "off"),
		"notifications": cmp.Or(m.prefs["notifications"], "on"),
	}
}

func (m *model) openSettings() {
	values := m.savedSettings()
	rows := settingRows
	if m.conn.product == "dal" {
		rows = slices.DeleteFunc(slices.Clone(rows), func(row settingRow) bool { return row.key == "editReview" })
	}
	m.settings = &settingsPanel{rows: rows, saved: values, preview: maps.Clone(values)}
}

func (m *model) showSetting(key, text string) {
	switch key {
	case "theme":
		m.screen.themeSetting = text
		m.screen.theme = theme{}
		m.screen.setTheme()
	case "verbosity":
		m.screen.verbosity = text
	case "icons":
		m.screen.iconsSetting = text
	case "statusHide":
		m.screen.statusHide = hiddenIDs(text)
	}
}

func (m *model) showSettings(values map[string]string) {
	for _, key := range []string{"theme", "verbosity", "icons", "statusHide"} {
		m.showSetting(key, values[key])
	}
}

func (m *model) closeSettings() {
	if m.settings != nil {
		m.showSettings(m.settings.saved)
	}
	m.settings = nil
}

func (m *model) settingsKey(msg tea.KeyPressMsg) tea.Cmd {
	p := m.settings
	switch msg.String() {
	case "up":
		p.focus = max(p.focus-1, 0)
	case "down":
		p.focus = min(p.focus+1, len(p.rows)-1)
	case "left":
		m.stepSetting(-1)
	case "right":
		m.stepSetting(1)
	case "space":
		m.toggleSetting()
	case "enter":
		return m.saveSettings()
	case "esc":
		m.closeSettings()
	}
	return nil
}

func (m *model) focusedRow() settingRow {
	return m.settings.rows[m.settings.focus]
}

func (m *model) stepSetting(step int) {
	p := m.settings
	if m.focusedRow().key == "statusHide" {
		p.statusItem = (p.statusItem + step + len(statusItemIDs)) % len(statusItemIDs)
		return
	}
	m.cycleValue(m.focusedRow(), step)
}

func (m *model) cycleValue(row settingRow, step int) {
	options := row.options
	index := slices.Index(options, m.settings.preview[row.key])
	next := options[(index+step+len(options))%len(options)]
	m.settings.preview[row.key] = next
	m.showSetting(row.key, next)
}

func (m *model) toggleSetting() {
	p := m.settings
	if m.focusedRow().key != "statusHide" {
		m.cycleValue(m.focusedRow(), 1)
		return
	}
	id := statusItemIDs[p.statusItem]
	ids := hiddenIDs(p.preview["statusHide"])
	if slices.Contains(ids, id) {
		ids = slices.DeleteFunc(ids, func(s string) bool { return s == id })
	} else {
		ids = append(ids, id)
	}
	p.preview["statusHide"] = hiddenText(ids)
	m.showSetting("statusHide", p.preview["statusHide"])
}

func (m *model) saveSettings() tea.Cmd {
	if m.saving != nil {
		return nil
	}
	edits := m.settings.edits()
	if len(edits) == 0 {
		m.closeSettings()
		return nil
	}
	m.saving = m.settings.clone()
	return m.call("settingsSave", "config/batchWrite", settingsBatch{Edits: edits}, new(rpc.ConfigWriteResponse), "")
}

func (m *model) applySettingsSaved(msg resultMsg) tea.Cmd {
	saved := m.saving
	m.saving = nil
	m.settings = nil
	if saved == nil {
		return nil
	}
	m.showSettings(saved.preview)
	for _, key := range []string{"approval", "editReview", "notifications"} {
		m.setPref(key, saved.preview[key])
	}
	if saved.preview["approval"] != saved.saved["approval"] {
		m.noticef("saved; open threads keep their approval and sandbox")
		return nil
	}
	path := ""
	if response, ok := msg.value.(*rpc.ConfigWriteResponse); ok {
		path = response.FilePath
	}
	m.noticef("Saved to %s", path)
	return nil
}

func (m *model) settingsFailed(msg resultMsg) bool {
	if msg.op != "settingsSave" {
		return false
	}
	m.saving = nil
	text := fmt.Sprintf("Could not save settings: %v", msg.err)
	if m.settings == nil {
		m.errorf("%s", text)
		return true
	}
	m.settings.saveErr = text
	return true
}

func (m *model) settingsView() panelView {
	p := m.settings
	body := make([]string, 0, len(p.rows)+1+detailRows)
	for _, row := range p.rows {
		body = append(body, settingLine(row, p.preview[row.key]))
	}
	body = append(body, "")
	body = append(body, m.settingsDetail()...)
	return panelView{kind: "settings", layout: panelSettings, title: "Settings", body: body, actions: []string{settingsHint}, cursor: p.focus}
}

func settingLine(row settingRow, value string) string {
	if row.key == "statusHide" {
		if value == "" {
			value = "none"
		}
		return fmt.Sprintf("%-14s%s hidden", row.label, strings.ReplaceAll(value, ",", ", "))
	}
	return fmt.Sprintf("%-14s%s", row.label, value)
}

func (m *model) settingsDetail() []string {
	switch m.focusedRow().key {
	case "icons":
		return iconDetail()
	case "statusHide":
		return m.statusDetail()
	}
	return make([]string, detailRows)
}

func iconDetail() []string {
	rows := []string{fmt.Sprintf("%-14s%-18s%s", "", "Unicode", "Nerd")}
	rows = append(rows,
		fmt.Sprintf("%-14s%-18s%s", "Effort", strings.Join(unicodeIcons.effort[:], " "), strings.Join(nerdIcons.effort[:], " ")),
		fmt.Sprintf("%-14s%-18s%s", "Approval", unicodeIcons.approval["ask"]+" "+unicodeIcons.approval["auto"]+" "+unicodeIcons.approval["yolo"], nerdIcons.approval["ask"]+" "+nerdIcons.approval["auto"]+" "+nerdIcons.approval["yolo"]),
		fmt.Sprintf("%-14s%-18s%s", "Sandbox", unicodeIcons.sandbox["workspace-write"]+" "+unicodeIcons.sandbox["off"], nerdIcons.sandbox["workspace-write"]+" "+nerdIcons.sandbox["off"]),
		fmt.Sprintf("%-14s%-18s%s", "Folder", unicodeIcons.folder, nerdIcons.folder),
		fmt.Sprintf("%-14s%-18s%s", "Queue", unicodeIcons.queue+"N", nerdIcons.queue+" N"),
		fmt.Sprintf("%-14s%-18s%s", "Context", "25%", string(moonGlyphs[int(0.25*moonSteps+0.5)])+" 25%"),
	)
	return rows
}

func (m *model) statusDetail() []string {
	hidden := hiddenIDs(m.settings.preview["statusHide"])
	rows := make([]string, 0, len(statusItemIDs))
	for i, id := range statusItemIDs {
		mark := "[ ]"
		if slices.Contains(hidden, id) {
			mark = "[x]"
		}
		prefix := "  "
		if i == m.settings.statusItem {
			prefix = "> "
		}
		rows = append(rows, prefix+mark+" "+id)
	}
	return rows
}
