package tui

import (
	"hash/fnv"
	"strconv"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
)

type role int

const (
	roleText role = iota
	roleDim
	roleFaint
	roleAccentText
	roleFailedText
	roleAddedText
	roleWarningText
	roleCount
)

type themeName string

const (
	themeNight     themeName = "night"
	themeSoftNight themeName = "soft-night"
	themeDay       themeName = "day"
	themeSoftDay   themeName = "soft-day"
)

var palettes = map[themeName][roleCount]uint32{
	themeNight: {
		roleText: 0xCECDC3, roleDim: 0x9F9D96, roleFaint: 0x6F6E69, roleAccentText: 0xDA702C,
		roleFailedText: 0xE8705F, roleAddedText: 0x879A39, roleWarningText: 0xD0A215,
	},
	themeSoftNight: {
		roleText: 0xCECDC3, roleDim: 0x9F9D96, roleFaint: 0x6F6E69, roleAccentText: 0xEC8B49,
		roleFailedText: 0xF89A8A, roleAddedText: 0xA0AF54, roleWarningText: 0xD0A215,
	},
	themeDay: {
		roleText: 0x100F0F, roleDim: 0x403E3C, roleFaint: 0x6F6E69, roleAccentText: 0x71320D,
		roleFailedText: 0x6C201C, roleAddedText: 0x3D4C07, roleWarningText: 0x664D01,
	},
	themeSoftDay: {
		roleText: 0x100F0F, roleDim: 0x403E3C, roleFaint: 0x6F6E69, roleAccentText: 0x71320D,
		roleFailedText: 0x6C201C, roleAddedText: 0x3D4C07, roleWarningText: 0x664D01,
	},
}

var nightHues = [4][2]uint32{
	{0xEC8B49, 0x100F0F}, {0x3AA99F, 0x100F0F}, {0x66A0C8, 0x100F0F}, {0xE47DA8, 0x100F0F},
}

var dayHues = [4][2]uint32{
	{0x9D4310, 0xFFFCF0}, {0x1C6C66, 0xFFFCF0}, {0x1A4F8C, 0xFFFCF0}, {0x87285E, 0xFFFCF0},
}

var hues = map[themeName][4][2]uint32{
	themeNight:     nightHues,
	themeSoftNight: nightHues,
	themeDay:       dayHues,
	themeSoftDay:   dayHues,
}

type theme struct {
	name  themeName
	known bool
	color bool
}

func newTheme(name themeName, color bool) theme {
	return theme{name: name, known: true, color: color}
}

func isExplicitTheme(setting string) bool {
	switch themeName(setting) {
	case themeNight, themeSoftNight, themeDay, themeSoftDay:
		return true
	}
	return false
}

func resolveTheme(setting string, dark bool) themeName {
	switch setting {
	case "night", "soft-night", "day", "soft-day":
		return themeName(setting)
	case "auto-soft":
		if dark {
			return themeSoftNight
		}
		return themeSoftDay
	}
	if dark {
		return themeNight
	}
	return themeDay
}

func threadHue(threadID string) int {
	h := fnv.New32a()
	_, _ = h.Write([]byte(threadID))
	return int(h.Sum32() % 4)
}

func sgr(codes []string) string {
	return "\x1b[" + strings.Join(codes, ";") + "m"
}

const reset = "\x1b[0m"

func rgbCodes(prefix string, value uint32) []string {
	return []string{
		prefix, "2",
		strconv.Itoa(int((value >> 16) & 0xFF)),
		strconv.Itoa(int((value >> 8) & 0xFF)),
		strconv.Itoa(int(value & 0xFF)),
	}
}

func (t theme) codes(r role) []string {
	if !t.known || !t.color {
		if r == roleDim {
			return []string{"2"}
		}
		return nil
	}
	return rgbCodes("38", palettes[t.name][r])
}

func (t theme) paint(r role, s string) string {
	codes := t.codes(r)
	if len(codes) == 0 || s == "" {
		return s
	}
	return sgr(codes) + s + reset
}

func (t theme) paintBold(r role, s string) string {
	return sgr(append([]string{"1"}, t.codes(r)...)) + s + reset
}

func (t theme) liveTag(hue int, s string) string {
	if !t.known || !t.color {
		return sgr([]string{"1", "7"}) + s + reset
	}
	pair := hues[t.name][hue]
	codes := append([]string{"1"}, rgbCodes("38", pair[1])...)
	codes = append(codes, rgbCodes("48", pair[0])...)
	return sgr(codes) + s + reset
}

func (t theme) hueText(hue int, s string) string {
	if !t.known || !t.color {
		return s
	}
	return sgr(rgbCodes("38", hues[t.name][hue][0])) + s + reset
}

func (s *screenState) setTheme() {
	if isExplicitTheme(s.themeSetting) || s.hasDark {
		s.theme = newTheme(resolveTheme(s.themeSetting, s.dark), s.color)
	}
}

func (s *screenState) dockTheme() theme {
	if s.theme.known {
		return s.theme
	}
	return newTheme(resolveTheme(s.themeSetting, true), s.color)
}

func (t theme) mark(s string) string {
	attrs := []string{"1"}
	if t.known {
		attrs = append(attrs, "4")
	}
	return sgr(append(attrs, t.codes(roleAccentText)...)) + s + reset
}

func downsample(text string, profile colorprofile.Profile) string {
	var out strings.Builder
	writer := colorprofile.Writer{Forward: &out, Profile: profile}
	if _, err := writer.WriteString(text); err != nil {
		return ansi.Strip(text)
	}
	return out.String()
}

func (s *screenState) emit(text string) string {
	return downsample(text, s.profile)
}

func (m *model) applyEnv(msg tea.EnvMsg) {
	m.screen.env = msg
	m.screen.color = msg.Getenv("NO_COLOR") == ""
	if !m.screen.color {
		m.screen.profile = colorprofile.ASCII
	}
	m.applyMotion(msg)
	m.screen.setTheme()
}

func (m *model) applyColorProfile(msg tea.ColorProfileMsg) {
	if m.screen.color {
		m.screen.profile = msg.Profile
	}
}

func (m *model) updateColour(msg tea.Msg) (tea.Cmd, bool) {
	switch msg := msg.(type) {
	case tea.EnvMsg:
		return m.applyEnvMsg(msg), true
	case tea.CapabilityMsg:
		m.applyCapability(msg)
	case tea.ColorProfileMsg:
		m.applyColorProfile(msg)
	case tea.BackgroundColorMsg:
		m.applyBackground(msg)
	default:
		return nil, false
	}
	return nil, true
}

func (m *model) applyBackground(msg tea.BackgroundColorMsg) {
	m.screen.dark = msg.IsDark()
	m.screen.hasDark = true
	m.screen.setTheme()
}
