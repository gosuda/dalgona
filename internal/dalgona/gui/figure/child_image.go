package figure

import (
	"errors"
	"image"
	"math"
)

func imageJob(env envelope, data []byte) (*jobResult, error) {
	if len(env.Viewport) != 2 || !finiteScale(env.Scale) {
		return nil, fail(FailureProtocol, "image request needs a viewport and a scale")
	}
	img, err := decodeJobImage(env.Format, data)
	if err != nil {
		return nil, err
	}
	preview := ImagePreview(img, previewSide(env.Viewport, env.Scale))
	return imageTiles(preview, img.Bounds().Size())
}

func decodeJobImage(format string, data []byte) (*image.NRGBA, error) {
	header, err := checkJobImage(format, data)
	if err != nil {
		return nil, err
	}
	img, err := decodeImage(data, header)
	if err != nil {
		return nil, imageFailure(err)
	}
	return img, nil
}

func checkJobImage(format string, data []byte) (ImageHeader, error) {
	header, err := InspectImage(data)
	if err != nil {
		return ImageHeader{}, imageFailure(err)
	}
	if header.Format.String() != format {
		return ImageHeader{}, fail(FailureInvalid, "image bytes are %s but the request declares %q", header.Format, format)
	}
	return header, nil
}

func imageFailure(err error) error {
	if errors.Is(err, ErrImageLimit) {
		return fail(FailureLimit, "%v", err)
	}
	return fail(FailureInvalid, "%v", err)
}

func previewSide(viewport []float64, scale float64) int {
	side := math.Ceil(math.Max(viewport[0], viewport[1]) * scale)
	if math.IsNaN(side) || side < 1 {
		return 1
	}
	return int(math.Min(side, previewSideLimit))
}

func imageTiles(preview *image.NRGBA, source image.Point) (*jobResult, error) {
	size := preview.Bounds().Size()
	result := &jobResult{kind: resultImage, bounds: []float64{float64(size.X), float64(size.Y), float64(source.X), float64(source.Y)}}
	for y := 0; y < size.Y; y += TileSideLimit {
		for x := 0; x < size.X; x += TileSideLimit {
			r := image.Rect(x, y, min(x+TileSideLimit, size.X), min(y+TileSideLimit, size.Y)).Add(preview.Bounds().Min)
			tile, err := ImageTile(preview, r)
			if err != nil {
				return nil, fail(FailureEngine, "image tile failed: %v", err)
			}
			result.chunks = append(result.chunks, resultChunk{tile: []int{x, y, r.Dx(), r.Dy()}, payload: premultiplied(tile)})
		}
	}
	return result, nil
}

func premultiplied(src *image.NRGBA) []byte {
	b := src.Bounds()
	out := make([]byte, 0, b.Dx()*b.Dy()*4)
	for y := b.Min.Y; y < b.Max.Y; y++ {
		row := src.Pix[(y-b.Min.Y)*src.Stride:]
		for x := range b.Dx() {
			p := row[x*4 : x*4+4]
			out = append(out, premultipliedChannel(p[0], p[3]), premultipliedChannel(p[1], p[3]), premultipliedChannel(p[2], p[3]), p[3])
		}
	}
	return out
}

func premultipliedChannel(c, alpha uint8) uint8 {
	return uint8(min(uint16(c)*uint16(alpha)/255, math.MaxUint8))
}
