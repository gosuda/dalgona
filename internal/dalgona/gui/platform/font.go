//go:build gui

package platform

import (
	_ "embed"
	"fmt"
	"sync"

	giofont "gioui.org/font"
	"gioui.org/font/opentype"
	"gioui.org/text"

	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	TitleFace giofont.Typeface = `"Bricolage Grotesque", Pretendard`
	ProseFace giofont.Typeface = `"Hanken Grotesk", Pretendard`
	CodeFace  giofont.Typeface = "monospace"
	IconFace  giofont.Typeface = `"Symbols Nerd Font Mono"`
)

var (
	//go:embed fontfaces/BricolageGrotesque-600.ttf
	bricolage600 []byte
	//go:embed fontfaces/BricolageGrotesque-650.ttf
	bricolage650 []byte
	//go:embed fontfaces/BricolageGrotesque-700.ttf
	bricolage700 []byte
	//go:embed fontfaces/HankenGrotesk-600.ttf
	hanken600 []byte
	//go:embed fontfaces/HankenGrotesk-700.ttf
	hanken700 []byte
	//go:embed fontfaces/Pretendard-600.otf
	pretendard600 []byte
	//go:embed fontfaces/Pretendard-700.otf
	pretendard700 []byte
	//go:embed fontfaces/SymbolsNerdSubset.ttf
	symbolsNerd []byte
)

type bundledFace struct {
	family giofont.Typeface
	weight theme.Weight
	data   []byte
}

var fontCatalog = [...]bundledFace{
	{"Bricolage Grotesque", theme.WeightTitle, bricolage600},
	{"Bricolage Grotesque", theme.WeightControl, bricolage650},
	{"Bricolage Grotesque", theme.WeightStrong, bricolage700},
	{"Hanken Grotesk", theme.WeightText, hanken400},
	{"Hanken Grotesk", theme.WeightTitle, hanken600},
	{"Hanken Grotesk", theme.WeightStrong, hanken700},
	{"Pretendard", theme.WeightText, pretendard400},
	{"Pretendard", theme.WeightTitle, pretendard600},
	{"Pretendard", theme.WeightStrong, pretendard700},
	{"Symbols Nerd Font Mono", theme.WeightText, symbolsNerd},
}

var FontCollection = sync.OnceValues(parseFontCatalog)

func parseFontCatalog() ([]giofont.FontFace, error) {
	faces := make([]giofont.FontFace, 0, len(fontCatalog))
	for _, bundled := range fontCatalog {
		face, err := opentype.Parse(bundled.data)
		if err != nil {
			return nil, fmt.Errorf("parse bundled font %s %d: %w", bundled.family, bundled.weight, err)
		}
		faces = append(faces, giofont.FontFace{
			Font: giofont.Font{Typeface: bundled.family, Weight: FontWeight(bundled.weight)},
			Face: face,
		})
	}
	return faces, nil
}

func FontWeight(w theme.Weight) giofont.Weight {
	return giofont.Weight(w - theme.WeightText)
}

func NewShaper() (*text.Shaper, error) {
	faces, err := FontCollection()
	if err != nil {
		return nil, err
	}
	return text.NewShaper(text.WithCollection(faces)), nil
}
