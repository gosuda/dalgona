package theme

import (
	"image/color"
	"math"
	"testing"
)

func wcagChannel(c uint8) float64 {
	s := float64(c) / 255
	if s <= 0.04045 {
		return s / 12.92
	}
	return math.Pow((s+0.055)/1.055, 2.4)
}

func wcagLuminance(c color.NRGBA) float64 {
	return 0.2126*wcagChannel(c.R) + 0.7152*wcagChannel(c.G) + 0.0722*wcagChannel(c.B)
}

func wcagRatio(a, b color.NRGBA) float64 {
	la, lb := wcagLuminance(a), wcagLuminance(b)
	return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
}

func TestContrastOracleMatchesWCAGReferences(t *testing.T) {
	black := color.NRGBA{A: 0xff}
	white := color.NRGBA{R: 0xff, G: 0xff, B: 0xff, A: 0xff}
	grey := color.NRGBA{R: 0x76, G: 0x76, B: 0x76, A: 0xff}
	if got := wcagRatio(black, white); math.Abs(got-21) > 1e-9 {
		t.Fatalf("black on white = %v, want 21", got)
	}
	if got := wcagRatio(grey, white); got < 4.54 || got >= 4.55 {
		t.Fatalf("#767676 on white = %v, want 4.54", got)
	}
}

type roleOf func(Palette) color.NRGBA

var (
	textRoles = map[string]roleOf{
		"text":         func(p Palette) color.NRGBA { return p.Text },
		"dim":          func(p Palette) color.NRGBA { return p.Dim },
		"accent-text":  func(p Palette) color.NRGBA { return p.AccentText },
		"failed-text":  func(p Palette) color.NRGBA { return p.FailedText },
		"added-text":   func(p Palette) color.NRGBA { return p.AddedText },
		"warning-text": func(p Palette) color.NRGBA { return p.WarningText },
	}
	groundRoles = map[string]roleOf{
		"canvas":  func(p Palette) color.NRGBA { return p.Canvas },
		"surface": func(p Palette) color.NRGBA { return p.Surface },
		"tone":    func(p Palette) color.NRGBA { return p.Tone },
	}
	fixedThemes = []Name{Night, SoftNight, Day, SoftDay}
)

type contrastRule struct {
	fg, bg string
	min    float64
}

func contrastRules() []contrastRule {
	var rules []contrastRule
	for bg := range groundRoles {
		for fg := range textRoles {
			rules = append(rules, contrastRule{fg: fg, bg: bg, min: 4.5})
		}
		rules = append(rules, contrastRule{fg: "strong", bg: bg, min: 3})
	}
	return rules
}

func role(name string) roleOf {
	if name == "strong" {
		return func(p Palette) color.NRGBA { return p.Strong }
	}
	if r, ok := textRoles[name]; ok {
		return r
	}
	return groundRoles[name]
}

func TestEveryThemeMeetsTextAndEdgeContrast(t *testing.T) {
	for _, name := range fixedThemes {
		p := name.Palette()
		for _, rule := range contrastRules() {
			if r := wcagRatio(role(rule.fg)(p), role(rule.bg)(p)); r < rule.min {
				t.Errorf("theme %d %s on %s = %.2f, want at least %v", name, rule.fg, rule.bg, r, rule.min)
			}
		}
	}
}

func TestEveryThreadHueHasLegibleInkAndVisibleFill(t *testing.T) {
	for _, name := range fixedThemes {
		p := name.Palette()
		for index := range HueCount {
			hue := name.hueAt(index)
			if r := wcagRatio(hue.Ink, hue.Fill); r < 4.5 {
				t.Errorf("%v hue %d ink on fill = %.2f, want at least 4.5", name, index, r)
			}
			if r := wcagRatio(hue.Fill, p.Canvas); r < 3 {
				t.Errorf("%v hue %d fill on canvas = %.2f, want at least 3", name, index, r)
			}
		}
	}
}

func fnv1a32(s string) uint32 {
	h := uint32(2166136261)
	for i := range len(s) {
		h ^= uint32(s[i])
		h *= 16777619
	}
	return h
}

func TestThreadHueIndexIsFNV1aOfUTF8Bytes(t *testing.T) {
	ids := []string{"a", "thread-1", "019a2b3c-4d5e-7f80-9a1b-2c3d4e5f6071", "스레드", "👩‍💻", "t\x00z"}
	for _, id := range ids {
		want := int(fnv1a32(id) % 4)
		got, ok := ThreadHueIndex(id)
		if !ok || got != want {
			t.Errorf("ThreadHueIndex(%q) = %d, %v; want %d, true", id, got, ok, want)
		}
		if again, _ := ThreadHueIndex(id); again != got {
			t.Errorf("ThreadHueIndex(%q) changed from %d to %d", id, got, again)
		}
	}
}

func TestThreadHueFollowsTheNightOrDayPair(t *testing.T) {
	id := "thread-1"
	night, okNight := Night.ThreadHue(id)
	softNight, okSoftNight := SoftNight.ThreadHue(id)
	day, okDay := Day.ThreadHue(id)
	softDay, okSoftDay := SoftDay.ThreadHue(id)
	if !okNight || !okSoftNight || !okDay || !okSoftDay {
		t.Fatal("an open thread carries no hue")
	}
	if night != softNight {
		t.Errorf("soft-night hue %v differs from night hue %v", softNight, night)
	}
	if day != softDay {
		t.Errorf("soft-day hue %v differs from day hue %v", softDay, day)
	}
	if night == day {
		t.Errorf("night and day share hue %v", night)
	}
}

func TestViewWithoutThreadCarriesNoHue(t *testing.T) {
	if hue, ok := Night.ThreadHue(""); ok {
		t.Fatalf("empty thread id carries hue %v", hue)
	}
}

func TestPointUsesThreadHueElseTheBrowserEmptyViewCyan(t *testing.T) {
	cyanNight := color.NRGBA{R: 0x3A, G: 0xA9, B: 0x9F, A: 0xff}
	cyanDay := color.NRGBA{R: 0x1C, G: 0x6C, B: 0x66, A: 0xff}
	if got := Night.Point("").Fill; got != cyanNight {
		t.Errorf("night empty-view point = %v, want cyan %v", got, cyanNight)
	}
	if got := SoftDay.Point("").Fill; got != cyanDay {
		t.Errorf("soft-day empty-view point = %v, want cyan %v", got, cyanDay)
	}
	for _, id := range []string{"a", "b", "c", "d", "e"} {
		hue, _ := Day.ThreadHue(id)
		if got := Day.Point(id); got != hue {
			t.Errorf("point of thread %q = %v, want its hue %v", id, got, hue)
		}
	}
}

func TestStoredSettingParsesAndResolves(t *testing.T) {
	cases := []struct {
		stored string
		dark   bool
		want   Name
	}{
		{"auto", true, Night},
		{"auto", false, Day},
		{"auto-soft", true, SoftNight},
		{"auto-soft", false, SoftDay},
		{"night", false, Night},
		{"soft-night", false, SoftNight},
		{"day", true, Day},
		{"soft-day", true, SoftDay},
		{"sepia", true, Night},
		{"", false, Day},
	}
	for _, c := range cases {
		if got := ParseSetting(c.stored).Resolve(c.dark); got != c.want {
			t.Errorf("ParseSetting(%q).Resolve(%v) = %v, want %v", c.stored, c.dark, got, c.want)
		}
	}
	if got := ParseSetting("sepia"); got != SettingAuto {
		t.Errorf("unknown setting displays as %q, want auto", got)
	}
}

func TestSpaceTokensFollowFourPixelSteps(t *testing.T) {
	steps := []Dp{Space1, Space2, Space3, Space4, Space5, Space6, Space7, Space8}
	for i, value := range steps {
		if want := Dp(4 * (i + 1)); value != want {
			t.Errorf("space%d = %v, want %v", i+1, value, want)
		}
	}
}

func TestRemOriginTokensUseTheSixteenPixelBaseline(t *testing.T) {
	rems := []struct {
		name  string
		value Dp
		rem   float32
	}{
		{"sidebarMinWidth", SidebarMinWidth, 16},
		{"sidebarMaxWidth", SidebarMaxWidth, 19},
		{"transcriptMaxWidth", TranscriptMaxWidth, 56},
		{"shellStackWidth", ShellStackWidth, 52},
		{"threadListStackCap", ThreadListStackCap, 18},
		{"statusCompactWidth", StatusCompactWidth, 40},
		{"statusDropFolderWidth", StatusDropFolderWidth, 30},
		{"statusDropQueueWidth", StatusDropQueueWidth, 26},
		{"statusDropEffortWidth", StatusDropEffortWidth, 16},
		{"statusDropMarkWidth", StatusDropMarkWidth, 12},
		{"modalMaxWidth", ModalMaxWidth, 70},
		{"idleDockClearance", IdleDockClearance, 6},
		{"titleBasis", TitleBasis, 16},
		{"bandMinWidth", BandMinWidth, 8},
	}
	for _, r := range rems {
		if want := Dp(r.rem * 16); r.value != want {
			t.Errorf("%s = %v, want %v from %vrem", r.name, r.value, want, r.rem)
		}
	}
}

func TestHighContrastDrawsEdgesInTextInkAndKeepsEveryOtherRole(t *testing.T) {
	for _, n := range fixedThemes {
		base, contrast := n.Palette(), n.WithHighContrast().Palette()
		if contrast.Line != base.Text || contrast.Strong != base.Text {
			t.Errorf("theme %d high contrast edges line %v strong %v, want text %v", n, contrast.Line, contrast.Strong, base.Text)
		}
		contrast.Line, contrast.Strong = base.Line, base.Strong
		if contrast != base {
			t.Errorf("theme %d high contrast changed a role other than the edges", n)
		}
		if got, want := n.WithHighContrast().Point("t1"), n.Point("t1"); got != want {
			t.Errorf("theme %d high contrast changed the thread hue %v to %v", n, want, got)
		}
		if n.WithHighContrast().WithHighContrast() != n.WithHighContrast() {
			t.Errorf("theme %d high contrast is not idempotent", n)
		}
	}
}
