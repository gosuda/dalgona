//go:build gui

package math

import (
	"image/color"
	"testing"

	"gioui.org/op"
)

func TestDrawPaintsAMeasuredLayout(t *testing.T) {
	out, err := testFont(t).Layout(mustParse(t, `\text{ab}+\frac{a}{b}\tag{1}`), Options{Size: 20, Text: documentText(t)})
	if err != nil {
		t.Fatal(err)
	}
	if len(out.Glyphs) == 0 || len(out.Rules) == 0 || len(out.Texts) != 2 {
		t.Fatalf("layout items %+v", out)
	}
	ops := new(op.Ops)
	out.Draw(ops, color.NRGBA{A: 0xff})
}
