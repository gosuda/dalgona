package theme

import (
	"encoding/binary"
	"hash/fnv"
	"image/color"
)

type Setting string

const (
	SettingAuto      Setting = "auto"
	SettingAutoSoft  Setting = "auto-soft"
	SettingNight     Setting = "night"
	SettingSoftNight Setting = "soft-night"
	SettingDay       Setting = "day"
	SettingSoftDay   Setting = "soft-day"
)

func ParseSetting(stored string) Setting {
	switch s := Setting(stored); s {
	case SettingAuto, SettingAutoSoft, SettingNight, SettingSoftNight, SettingDay, SettingSoftDay:
		return s
	default:
		return SettingAuto
	}
}

func (s Setting) Resolve(systemDark bool) Name {
	switch s {
	case SettingNight:
		return Night
	case SettingSoftNight:
		return SoftNight
	case SettingDay:
		return Day
	case SettingSoftDay:
		return SoftDay
	case SettingAutoSoft:
		return pick(systemDark, SoftNight, SoftDay)
	case SettingAuto:
		return pick(systemDark, Night, Day)
	}
	return pick(systemDark, Night, Day)
}

func pick(dark bool, night, day Name) Name {
	if dark {
		return night
	}
	return day
}

type Name uint8

const (
	Night Name = iota
	SoftNight
	Day
	SoftDay
)

const highContrast Name = 1 << 7

func (n Name) WithHighContrast() Name { return n | highContrast }

func (n Name) base() Name { return n &^ highContrast }

func (n Name) isNight() bool {
	return n.base() == Night || n.base() == SoftNight
}

type Palette struct {
	Canvas      color.NRGBA
	Surface     color.NRGBA
	Tone        color.NRGBA
	Line        color.NRGBA
	Strong      color.NRGBA
	Text        color.NRGBA
	Dim         color.NRGBA
	Faint       color.NRGBA
	AccentText  color.NRGBA
	FailedText  color.NRGBA
	AddedText   color.NRGBA
	WarningText color.NRGBA
}

func rgb(hex uint32) color.NRGBA {
	var b [4]byte
	binary.BigEndian.PutUint32(b[:], hex)
	return color.NRGBA{R: b[1], G: b[2], B: b[3], A: 0xff}
}

var palettes = [...]Palette{
	Night: {
		Canvas: rgb(0x111113), Surface: rgb(0x18191b), Tone: rgb(0x212225), Line: rgb(0x363a3f),
		Strong: rgb(0x696e77), Text: rgb(0xCECDC3), Dim: rgb(0x9F9D96), Faint: rgb(0x6F6E69),
		AccentText: rgb(0xDA702C), FailedText: rgb(0xE8705F), AddedText: rgb(0x879A39), WarningText: rgb(0xD0A215),
	},
	SoftNight: {
		Canvas: rgb(0x212225), Surface: rgb(0x272a2d), Tone: rgb(0x2e3135), Line: rgb(0x43484e),
		Strong: rgb(0x777b84), Text: rgb(0xCECDC3), Dim: rgb(0x9F9D96), Faint: rgb(0x6F6E69),
		AccentText: rgb(0xEC8B49), FailedText: rgb(0xF89A8A), AddedText: rgb(0xA0AF54), WarningText: rgb(0xD0A215),
	},
	Day: {
		Canvas: rgb(0xFFFCF0), Surface: rgb(0xE6E4D9), Tone: rgb(0xCECDC3), Line: rgb(0xB7B5AC),
		Strong: rgb(0x6F6E69), Text: rgb(0x100F0F), Dim: rgb(0x403E3C), Faint: rgb(0x6F6E69),
		AccentText: rgb(0x71320D), FailedText: rgb(0x6C201C), AddedText: rgb(0x3D4C07), WarningText: rgb(0x664D01),
	},
	SoftDay: {
		Canvas: rgb(0xE6E4D9), Surface: rgb(0xFFFCF0), Tone: rgb(0xCECDC3), Line: rgb(0xB7B5AC),
		Strong: rgb(0x6F6E69), Text: rgb(0x100F0F), Dim: rgb(0x403E3C), Faint: rgb(0x6F6E69),
		AccentText: rgb(0x71320D), FailedText: rgb(0x6C201C), AddedText: rgb(0x3D4C07), WarningText: rgb(0x664D01),
	},
}

func (n Name) Palette() Palette {
	p := palettes[n.base()]
	if n&highContrast != 0 {
		p.Line, p.Strong = p.Text, p.Text
	}
	return p
}

type Hue struct {
	Fill color.NRGBA
	Ink  color.NRGBA
}

const HueCount = 4

var (
	nightHues = [HueCount]Hue{
		{Fill: rgb(0xEC8B49), Ink: rgb(0x100F0F)},
		{Fill: rgb(0x3AA99F), Ink: rgb(0x100F0F)},
		{Fill: rgb(0x66A0C8), Ink: rgb(0x100F0F)},
		{Fill: rgb(0xE47DA8), Ink: rgb(0x100F0F)},
	}
	dayHues = [HueCount]Hue{
		{Fill: rgb(0x9D4310), Ink: rgb(0xFFFCF0)},
		{Fill: rgb(0x1C6C66), Ink: rgb(0xFFFCF0)},
		{Fill: rgb(0x1A4F8C), Ink: rgb(0xFFFCF0)},
		{Fill: rgb(0x87285E), Ink: rgb(0xFFFCF0)},
	}
)

func ThreadHueIndex(threadID string) (int, bool) {
	if threadID == "" {
		return 0, false
	}
	h := fnv.New32a()
	_, _ = h.Write([]byte(threadID))
	return int(h.Sum32() % HueCount), true
}

func (n Name) ThreadHue(threadID string) (Hue, bool) {
	index, ok := ThreadHueIndex(threadID)
	if !ok {
		return Hue{}, false
	}
	return n.hueAt(index), true
}

const emptyViewHue = 1

func (n Name) Point(threadID string) Hue {
	if hue, ok := n.ThreadHue(threadID); ok {
		return hue
	}
	return n.hueAt(emptyViewHue)
}

func (n Name) hueAt(index int) Hue {
	if n.isNight() {
		return nightHues[index]
	}
	return dayHues[index]
}
