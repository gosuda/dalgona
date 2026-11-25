package figure

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"sync"

	"oss.terrastruct.com/d2/d2renderers/d2fonts"

	figmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
)

type Fonts struct {
	Math []byte
	Text [][]byte
}

type formulaFaces struct {
	font *figmath.Font
	text *figmath.TextFont
}

type formulaSource func() (formulaFaces, error)

func (f Fonts) formulas() formulaSource {
	return sync.OnceValues(func() (formulaFaces, error) {
		if len(f.Math) == 0 || len(f.Text) == 0 {
			return formulaFaces{}, errors.New("the figure renderer received no math font; start it with the bundled fonts")
		}
		font, err := figmath.ParseFont(f.Math)
		if err != nil {
			return formulaFaces{}, err
		}
		text, err := figmath.NewTextFont(f.Text...)
		if err != nil {
			return formulaFaces{}, err
		}
		return formulaFaces{font: font, text: text}, nil
	})
}

func (f Fonts) manifest() string {
	h := sha256.New()
	faces := append(d2Faces(), f.Math)
	for _, face := range append(faces, f.Text...) {
		h.Write(binary.LittleEndian.AppendUint64(nil, uint64(len(face))))
		h.Write(face)
	}
	return hex.EncodeToString(h.Sum(nil))
}

func d2Faces() [][]byte {
	var faces [][]byte
	for _, style := range [...]d2fonts.FontStyle{d2fonts.FONT_STYLE_REGULAR, d2fonts.FONT_STYLE_BOLD, d2fonts.FONT_STYLE_SEMIBOLD, d2fonts.FONT_STYLE_ITALIC} {
		for _, family := range [...]d2fonts.FontFamily{d2fonts.SourceSansPro, d2fonts.SourceCodePro} {
			face, _ := d2fonts.FontFaces.Lookup(family.Font(0, style))
			faces = append(faces, face)
		}
	}
	return faces
}
