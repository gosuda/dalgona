package math

import (
	"slices"
	"testing"
)

func deviceAt(device []byte) *mathTable {
	return &mathTable{reader: tableReader{data: append([]byte{0, 0}, device...)}}
}

func TestDeviceDeltasDecodeEveryFormat(t *testing.T) {
	cases := []struct {
		name   string
		device []byte
		start  int
		deltas []int
	}{
		{"2-bit", deviceTable(10, 17, 1, 0x785B), 10, []int{1, -1, -2, 0, 1, 1, -2, -1}},
		{"4-bit", deviceTable(5, 9, 2, 0x783F, 0x2000), 5, []int{7, -8, 3, -1, 2}},
		{"8-bit", deviceTable(30, 32, 3, 0x7F80, 0xFB00), 30, []int{127, -128, -5}},
	}
	for _, tc := range cases {
		table := deviceAt(tc.device)
		got := make([]int, 0, len(tc.deltas)+2)
		for ppem := tc.start - 1; ppem <= tc.start+len(tc.deltas); ppem++ {
			got = append(got, table.delta(2, ppem))
		}
		want := append(append([]int{0}, tc.deltas...), 0)
		if !slices.Equal(got, want) {
			t.Errorf("%s deltas from %d ppem = %v, want %v", tc.name, tc.start-1, got, want)
		}
		if adjusted := table.delta(0, tc.start); adjusted != 0 {
			t.Errorf("%s: a null device offset adjusted by %d", tc.name, adjusted)
		}
	}
}

func TestUnusableDevicesLeaveValuesUnchanged(t *testing.T) {
	unusable := map[string][]byte{
		"variation index":  deviceTable(0, 0, 0x8000),
		"reserved format":  deviceTable(10, 40, 4, 0xFFFF, 0xFFFF),
		"truncated deltas": deviceTable(10, 40, 1, 0x5555),
		"truncated header": {0, 10, 0},
	}
	for name, device := range unusable {
		if got := deviceAt(device).delta(2, 35); got != 0 {
			t.Errorf("%s device adjusted by %d", name, got)
		}
	}
}

func TestDeviceTablesAdjustMetricsAtTheirPixelSize(t *testing.T) {
	font := testFont(t)
	lay := func(source string, size float32) *Layout {
		t.Helper()
		out, err := font.Layout(mustParse(t, source), Options{Size: size, Text: halfEmText{}})
		if err != nil {
			t.Fatalf("Layout(%q, %v): %v", source, size, err)
		}
		return out
	}
	for _, tc := range []struct{ size, axis, thickness float32 }{
		{20, 250*0.020 + 3, 60*0.020 + 1},
		{21, 250*0.021 - 2, 60 * 0.021},
		{22, 250 * 0.022, 60 * 0.022},
	} {
		rule := onlyRule(t, lay(`\frac{a}{b}`, tc.size))
		near(t, "fraction rule top", -rule.Y, tc.axis+tc.thickness/2)
		near(t, "fraction rule thickness", rule.Height, tc.thickness)
	}
	integral := lay(`\int^a`, 20)
	near(t, "superscript after the adjusted italic correction", only(t, integral, "a").X, 500*0.02+150*0.02-1)
	kerned := lay(`f^a`, 20)
	near(t, "superscript after the adjusted kern", only(t, kerned, "a").X, 500*0.02+100*0.02+(-30*0.02-2))
}
