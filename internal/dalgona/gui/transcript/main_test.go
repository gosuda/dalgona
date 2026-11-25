//go:build gui

package transcript

import (
	"os"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func figureFonts() figure.Fonts {
	data, err := platform.MathFontData()
	if err != nil {
		panic(err)
	}
	return figure.Fonts{Math: data, Text: platform.MathTextFaceData()}
}

func TestMain(m *testing.M) {
	if figure.IsChild(os.Args) {
		os.Exit(figure.ServeChild(figureFonts()))
	}
	os.Exit(m.Run())
}
