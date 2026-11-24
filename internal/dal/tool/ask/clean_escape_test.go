package ask

import "testing"

func TestCleanEscapeProfile(t *testing.T) {
	esc := "\x1b"
	bel := "\x07"
	can := "\x18"
	sub := "\x1a"
	hangul := "한"
	visible := "\\u001B"
	cases := []struct {
		name string
		in   string
		want string
	}{
		{"csi params intermediates final stripped", esc + "[1;31mRED" + esc + "[0m", "RED"},
		{"csi intermediate only stripped", esc + "[$mX", "X"},
		{"csi param after intermediate invalid", esc + "[$0mX", visible + "[$0mX"},
		{"csi control byte invalid", esc + "[31" + bel + "mX", visible + "[31mX"},
		{"csi incomplete at end visible", "A" + esc + "[", "A" + visible + "["},
		{"csi invalid preserves multibyte", esc + "[" + hangul + "X", visible + "[" + hangul + "X"},
		{"apc st stripped", esc + "_app" + esc + "\\y", "y"},
		{"sos st stripped", esc + "Xhello" + esc + "\\y", "y"},
		{"pm st stripped", esc + "^hello" + esc + "\\y", "y"},
		{"lone esc at end visible", "A" + esc, "A" + visible},
		{"esc backslash short stripped", "A" + esc + "\\B", "AB"},
		{"osc digits semicolon bel stripped", esc + "]0;title" + bel + "x", "x"},
		{"osc digits st empty payload stripped", esc + "]0;" + esc + "\\X", "X"},
		{"osc missing selector visible", esc + "]title" + bel + "X", visible + "]titleX"},
		{"osc letter selector visible", esc + "]R" + bel + "X", visible + "]RX"},
		{"osc digits garbage visible", esc + "]0x" + bel + "X", visible + "]0xX"},
		{"dcs header payload st stripped", esc + "P1$|payload" + esc + "\\X", "X"},
		{"dcs empty payload st stripped", esc + "P1$|" + esc + "\\X", "X"},
		{"dcs bel terminator invalid", esc + "P1$|payload" + bel + "X", visible + "P1$|payloadX"},
		{"dcs non ascii header invalid", esc + "P" + hangul + "|" + esc + "\\X", visible + "P" + hangul + "|X"},
		{"short intermediate final stripped", esc + "(BX", "X"},
		{"short bare final stripped", esc + "cX", "X"},
		{"short control final invalid", esc + bel + "X", visible + "X"},
		{"can inside osc invalidates", esc + "]0;ab" + can + "cd" + bel + "X", visible + "]0;abcdX"},
		{"sub inside dcs invalidates", esc + "P1$|ab" + sub + "sub" + esc + "\\X", visible + "P1$|absubX"},
		{"foreign esc inside sos invalidates", esc + "Xab" + esc + "[31mcd" + esc + "\\X", visible + "XabcdX"},
		{"esc before hangul preserved", "a" + esc + hangul + "b", "a" + visible + hangul + "b"},
		{"esc before supplementary preserved", "a" + esc + "𠀀b", "a" + visible + "𠀀b"},
		{"esc before combining preserved", "a" + esc + "́b", "a" + visible + "́b"},
		{"esc before private use preserved", "a" + esc + "󀀁b", "a" + visible + "󀀁b"},
		{"osc utf8 payload opaque stripped", esc + "]0;héllo" + bel + "X", "X"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := CleanLine(tc.in); got != tc.want {
				t.Fatalf("CleanLine(%q) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
}

func TestCleanEscapeSharedProjection(t *testing.T) {
	esc := "\x1b"
	bel := "\x07"
	visible := "\\u001B"
	cases := []struct {
		name string
		in   string
		want string
	}{
		{"valid csi empty both", esc + "[31m" + esc + "[0m", ""},
		{"valid osc empty both", esc + "]0;title" + bel, ""},
		{"invalid osc visible both", esc + "]title" + bel + "x", visible + "]titlex"},
		{"incomplete esc visible both", "A" + esc, "A" + visible},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := CleanLine(tc.in); got != tc.want {
				t.Fatalf("CleanLine(%q) = %q, want %q", tc.in, got, tc.want)
			}
			if got := CleanBlock(tc.in); got != tc.want {
				t.Fatalf("CleanBlock(%q) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
}
