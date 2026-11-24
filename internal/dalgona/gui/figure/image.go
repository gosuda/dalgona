package figure

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"image"
	"image/color"
	"image/gif"
	"image/jpeg"
	"image/png"
	"math"
)

type ImageFormat uint8

const (
	ImagePNG ImageFormat = iota + 1
	ImageJPEG
	ImageGIF
)

const (
	ImageEncodedLimit   = 5 << 20
	ImageCountLimit     = 10
	ImageAggregateLimit = 10 << 20
	ImagePixelLimit     = 32_000_000
	ImageSideLimit      = 16384
	ImagePixelByteLimit = 128 << 20
)

const rgbaPixelBytes = 4

var (
	ErrImageFormat = errors.New("unsupported image format")
	ErrImageHeader = errors.New("malformed image header")
	ErrImageLimit  = errors.New("image attachment limit exceeded")
	ErrImageDecode = errors.New("image decode failed")
)

var (
	errPNGTruncated  = fmt.Errorf("%w: the PNG ends before its first image data chunk; the file is truncated, so export or download it again", ErrImageHeader)
	errJPEGTruncated = fmt.Errorf("%w: the JPEG ends before its frame header; the file is truncated, so export or download it again", ErrImageHeader)
	errJPEGNoFrame   = fmt.Errorf("%w: the JPEG reaches its scan data or end without a frame header; export the image again", ErrImageHeader)
	errGIFTruncated  = fmt.Errorf("%w: the GIF ends before its trailer; the file is truncated, so export or download it again", ErrImageHeader)
)

var (
	imageFormatNames      = [...]string{ImagePNG: "png", ImageJPEG: "jpeg", ImageGIF: "gif"}
	imageFormatMediaTypes = [...]string{ImagePNG: "image/png", ImageJPEG: "image/jpeg", ImageGIF: "image/gif"}
)

var (
	pngSignature   = []byte("\x89PNG\r\n\x1a\n")
	jpegSignature  = []byte{0xFF, 0xD8, 0xFF}
	gif87Signature = []byte("GIF87a")
	gif89Signature = []byte("GIF89a")
	jfifIdentifier = []byte("JFIF\x00")
	adobeIdentity  = []byte("Adobe")
)

func (f ImageFormat) String() string {
	if f == 0 || int(f) >= len(imageFormatNames) {
		return "unknown"
	}
	return imageFormatNames[f]
}

func (f ImageFormat) MediaType() string {
	if f == 0 || int(f) >= len(imageFormatMediaTypes) {
		return "application/octet-stream"
	}
	return imageFormatMediaTypes[f]
}

type ImageHeader struct {
	Format      ImageFormat
	Width       int
	Height      int
	Frames      int
	PixelBytes  int64
	Orientation int
}

type ImageCandidate struct {
	Name string
	Data []byte
}

type ImageAdmission struct {
	Accepted []ImageCandidate
	Headers  []ImageHeader
	Failures []ImageFailure
}

type ImageFailure struct {
	Name string
	Err  error
}

func InspectImage(data []byte) (ImageHeader, error) {
	if len(data) > ImageEncodedLimit {
		return ImageHeader{}, fmt.Errorf("%w: the file has %d bytes and an image may have at most %d; attach a smaller image", ErrImageLimit, len(data), ImageEncodedLimit)
	}
	header, err := inspectImageFormat(data)
	if err != nil {
		return ImageHeader{}, err
	}
	if header.PixelBytes > ImagePixelByteLimit {
		return ImageHeader{}, fmt.Errorf("%w: decoding needs %d bytes of pixel storage and the limit is %d; resize the image or save it with 8 bits per channel", ErrImageLimit, header.PixelBytes, ImagePixelByteLimit)
	}
	return header, nil
}

func inspectImageFormat(data []byte) (ImageHeader, error) {
	switch {
	case bytes.HasPrefix(data, pngSignature):
		return inspectPNG(data)
	case bytes.HasPrefix(data, jpegSignature):
		return inspectJPEG(data)
	case bytes.HasPrefix(data, gif87Signature), bytes.HasPrefix(data, gif89Signature):
		return inspectGIF(data)
	}
	return ImageHeader{}, fmt.Errorf("%w: the file does not start with a PNG, JPEG, or GIF signature; attach a PNG, JPEG, or GIF image", ErrImageFormat)
}

func inspectImageSides(width, height int64) error {
	if width > ImageSideLimit || height > ImageSideLimit {
		return fmt.Errorf("%w: the image is %dx%d pixels and each side may have at most %d; resize the image", ErrImageLimit, width, height, ImageSideLimit)
	}
	if pixels := imageProduct(width, height); pixels > ImagePixelLimit {
		return fmt.Errorf("%w: the image has %d pixels and the limit is %d; resize the image", ErrImageLimit, pixels, ImagePixelLimit)
	}
	return nil
}

func imageProduct(a, b int64) int64 {
	if a != 0 && b > math.MaxInt64/a {
		return math.MaxInt64
	}
	return a * b
}

type pngPixelBytes struct{ opaque, keyed int64 }

var pngPixelLayouts = map[[2]byte]pngPixelBytes{
	{0, 1}: {1, 4}, {0, 2}: {1, 4}, {0, 4}: {1, 4}, {0, 8}: {1, 4}, {0, 16}: {2, 8},
	{2, 8}: {4, 4}, {2, 16}: {8, 8},
	{3, 1}: {1, 1}, {3, 2}: {1, 1}, {3, 4}: {1, 1}, {3, 8}: {1, 1},
	{4, 8}: {4, 4}, {4, 16}: {8, 8},
	{6, 8}: {4, 4}, {6, 16}: {8, 8},
}

func inspectPNG(data []byte) (ImageHeader, error) {
	fields, err := pngHeaderChunk(data)
	if err != nil {
		return ImageHeader{}, err
	}
	layout, err := pngLayout(fields)
	if err != nil {
		return ImageHeader{}, err
	}
	width, height := int64(binary.BigEndian.Uint32(fields)), int64(binary.BigEndian.Uint32(fields[4:]))
	if width == 0 || height == 0 || max(width, height) > math.MaxInt32 {
		return ImageHeader{}, fmt.Errorf("%w: the PNG declares %dx%d pixels and each side must be between 1 and %d; export the image again", ErrImageHeader, width, height, math.MaxInt32)
	}
	if err = inspectImageSides(width, height); err != nil {
		return ImageHeader{}, err
	}
	keyed, err := pngTransparencyKey(data[len(pngSignature)+25:])
	if err != nil {
		return ImageHeader{}, err
	}
	perPixel := layout.opaque
	if keyed {
		perPixel = layout.keyed
	}
	return ImageHeader{Format: ImagePNG, Width: int(width), Height: int(height), Frames: 1, PixelBytes: imageProduct(imageProduct(width, height), perPixel), Orientation: 1}, nil
}

func pngHeaderChunk(data []byte) ([]byte, error) {
	chunk := data[len(pngSignature):]
	if len(chunk) < 25 {
		return nil, errPNGTruncated
	}
	if binary.BigEndian.Uint32(chunk) != 13 || string(chunk[4:8]) != "IHDR" {
		return nil, fmt.Errorf("%w: the first PNG chunk is not a 13-byte IHDR; export the image again", ErrImageHeader)
	}
	if crc32.ChecksumIEEE(chunk[4:21]) != binary.BigEndian.Uint32(chunk[21:]) {
		return nil, fmt.Errorf("%w: the PNG IHDR checksum does not match; the file is damaged, so export it again", ErrImageHeader)
	}
	return chunk[8:21], nil
}

func pngLayout(fields []byte) (pngPixelBytes, error) {
	depth, colorType := fields[8], fields[9]
	layout, known := pngPixelLayouts[[2]byte{colorType, depth}]
	if !known {
		return pngPixelBytes{}, fmt.Errorf("%w: PNG color type %d does not allow bit depth %d; export the image again", ErrImageHeader, colorType, depth)
	}
	if fields[10] != 0 || fields[11] != 0 || fields[12] > 1 {
		return pngPixelBytes{}, fmt.Errorf("%w: the PNG uses compression %d, filter %d, or interlace %d, which PNG does not define; export the image again", ErrImageHeader, fields[10], fields[11], fields[12])
	}
	return layout, nil
}

func pngTransparencyKey(chunks []byte) (bool, error) {
	keyed := false
	for len(chunks) >= 8 {
		switch string(chunks[4:8]) {
		case "IDAT":
			return keyed, nil
		case "IEND":
			return false, fmt.Errorf("%w: the PNG ends before any image data; export the image again", ErrImageHeader)
		case "tRNS":
			keyed = true
		}
		length := uint64(binary.BigEndian.Uint32(chunks)) + 12
		if length > uint64(len(chunks)) {
			return false, errPNGTruncated
		}
		chunks = chunks[length:]
	}
	return false, errPNGTruncated
}

type jpegComponent struct{ id, h, v byte }

type jpegScan struct {
	jfif          bool
	adobeRGB      bool
	exifRead      bool
	orientation   int
	width, height int64
	count         int
	components    [4]jpegComponent
}

type jpegReader struct {
	data []byte
	pos  int
}

func inspectJPEG(data []byte) (ImageHeader, error) {
	reader := jpegReader{data: data, pos: len(jpegSignature) - 1}
	scan := jpegScan{orientation: 1}
	for {
		marker, segment, err := reader.next()
		if err != nil {
			return ImageHeader{}, err
		}
		if jpegFrameMarker(marker) {
			return scan.header(marker, segment)
		}
		scan.read(marker, segment)
	}
}

func jpegFrameMarker(marker byte) bool {
	return marker >= 0xC0 && marker <= 0xCF && marker != 0xC4 && marker != 0xC8 && marker != 0xCC
}

func (r *jpegReader) next() (byte, []byte, error) {
	for {
		marker, err := r.marker()
		switch {
		case err != nil:
			return 0, nil, err
		case marker == 0xD9, marker == 0xDA:
			return 0, nil, errJPEGNoFrame
		case marker == 0x01, marker >= 0xD0 && marker <= 0xD7:
			continue
		}
		segment, err := r.segment()
		return marker, segment, err
	}
}

func (r *jpegReader) marker() (byte, error) {
	for ; r.pos+1 < len(r.data); r.pos++ {
		if r.data[r.pos] == 0xFF && r.data[r.pos+1] != 0xFF && r.data[r.pos+1] != 0x00 {
			r.pos += 2
			return r.data[r.pos-1], nil
		}
	}
	return 0, errJPEGTruncated
}

func (r *jpegReader) segment() ([]byte, error) {
	if r.pos+2 > len(r.data) {
		return nil, errJPEGTruncated
	}
	length := int(binary.BigEndian.Uint16(r.data[r.pos:]))
	if length < 2 {
		return nil, fmt.Errorf("%w: a JPEG segment declares length %d, shorter than its own length field; export the image again", ErrImageHeader, length)
	}
	end := r.pos + length
	if end > len(r.data) {
		return nil, errJPEGTruncated
	}
	segment := r.data[r.pos+2 : end]
	r.pos = end
	return segment, nil
}

func (s *jpegScan) read(marker byte, segment []byte) {
	switch {
	case marker == 0xE0 && len(segment) >= len(jfifIdentifier):
		s.jfif = bytes.HasPrefix(segment, jfifIdentifier)
	case marker == 0xE1 && !s.exifRead:
		s.orientation, s.exifRead = exifOrientation(segment)
	case marker == 0xEE && len(segment) >= 12:
		s.adobeRGB = bytes.HasPrefix(segment, adobeIdentity) && segment[11] == 0
	}
}

func (s *jpegScan) header(marker byte, segment []byte) (ImageHeader, error) {
	if err := s.readFrame(marker, segment); err != nil {
		return ImageHeader{}, err
	}
	if err := inspectImageSides(s.width, s.height); err != nil {
		return ImageHeader{}, err
	}
	if err := s.readComponents(segment[6:]); err != nil {
		return ImageHeader{}, err
	}
	return ImageHeader{Format: ImageJPEG, Width: int(s.width), Height: int(s.height), Frames: 1, PixelBytes: s.pixelBytes(), Orientation: s.orientation}, nil
}

func (s *jpegScan) readFrame(marker byte, segment []byte) error {
	if err := jpegFrameCoding(marker, segment); err != nil {
		return err
	}
	s.count = int(segment[5])
	if len(segment) != 6+3*s.count {
		return fmt.Errorf("%w: the JPEG frame header has %d bytes but declares %d components; export the image again", ErrImageHeader, len(segment), s.count)
	}
	s.height, s.width = int64(binary.BigEndian.Uint16(segment[1:])), int64(binary.BigEndian.Uint16(segment[3:]))
	if s.width == 0 || s.height == 0 {
		return fmt.Errorf("%w: the JPEG declares %dx%d pixels; export the image again", ErrImageHeader, s.width, s.height)
	}
	return nil
}

func jpegFrameCoding(marker byte, segment []byte) error {
	switch {
	case marker > 0xC2:
		return fmt.Errorf("%w: the JPEG uses coding process %#x, which is neither baseline nor progressive; save it as a baseline JPEG", ErrImageFormat, marker)
	case len(segment) < 6:
		return fmt.Errorf("%w: the JPEG frame header has %d bytes, fewer than its 6 fixed bytes; export the image again", ErrImageHeader, len(segment))
	case segment[0] != 8:
		return fmt.Errorf("%w: the JPEG uses %d-bit samples; save it with 8-bit samples", ErrImageFormat, segment[0])
	case segment[5] != 1 && segment[5] != 3 && segment[5] != 4:
		return fmt.Errorf("%w: the JPEG has %d color components; save it as grayscale, RGB, or CMYK", ErrImageFormat, segment[5])
	}
	return nil
}

func (s *jpegScan) readComponents(fields []byte) error {
	for i := range s.count {
		component := jpegComponent{id: fields[3*i], h: fields[3*i+1] >> 4, v: fields[3*i+1] & 0x0F}
		if component.h < 1 || component.h > 4 || component.v < 1 || component.v > 4 {
			return fmt.Errorf("%w: JPEG component %d has sampling factors %dx%d outside 1 to 4; export the image again", ErrImageHeader, i, component.h, component.v)
		}
		if component.h == 3 || component.v == 3 {
			return fmt.Errorf("%w: JPEG component %d uses sampling factor 3; save the image with standard chroma subsampling", ErrImageFormat, i)
		}
		s.components[i] = component
	}
	return nil
}

func (s *jpegScan) pixelBytes() int64 {
	pixels := imageProduct(s.width, s.height)
	switch {
	case s.count == 1:
		return pixels
	case s.count == 4, s.rgb():
		return imageProduct(pixels, rgbaPixelBytes)
	}
	horizontal, vertical := s.chromaRatio()
	chroma := imageProduct((s.width+horizontal-1)/horizontal, (s.height+vertical-1)/vertical)
	return pixels + imageProduct(chroma, 2)
}

func (s *jpegScan) rgb() bool {
	if s.jfif {
		return false
	}
	return s.adobeRGB || s.components[0].id == 'R' && s.components[1].id == 'G' && s.components[2].id == 'B'
}

func (s *jpegScan) chromaRatio() (int64, int64) {
	luma, blue, red := s.components[0], s.components[1], s.components[2]
	maxH, maxV := max(luma.h, blue.h, red.h), max(luma.v, blue.v, red.v)
	if blue.h != red.h || blue.v != red.v || luma.h != maxH || luma.v != maxV || maxV/blue.v > 2 {
		return 1, 1
	}
	return int64(maxH / blue.h), int64(maxV / blue.v)
}

const (
	gifScreenEnd       = 13
	gifImageDescriptor = 0x2C
	gifExtension       = 0x21
	gifTrailer         = 0x3B
)

func inspectGIF(data []byte) (ImageHeader, error) {
	if len(data) < gifScreenEnd {
		return ImageHeader{}, errGIFTruncated
	}
	width, height := int64(binary.LittleEndian.Uint16(data[6:])), int64(binary.LittleEndian.Uint16(data[8:]))
	if width == 0 || height == 0 {
		return ImageHeader{}, fmt.Errorf("%w: the GIF screen is %dx%d pixels; export the image again", ErrImageHeader, width, height)
	}
	if err := inspectImageSides(width, height); err != nil {
		return ImageHeader{}, err
	}
	frames, err := gifFrames(data, gifScreenEnd+gifColorTableBytes(data[10]), image.Pt(int(width), int(height)))
	if err != nil {
		return ImageHeader{}, err
	}
	return ImageHeader{Format: ImageGIF, Width: int(width), Height: int(height), Frames: frames, PixelBytes: imageProduct(width, height), Orientation: 1}, nil
}

func gifColorTableBytes(flags byte) int {
	if flags&0x80 == 0 {
		return 0
	}
	return 3 << (flags&0x07 + 1)
}

func gifFrames(data []byte, pos int, screen image.Point) (int, error) {
	frames := 0
	for pos < len(data) {
		var err error
		switch data[pos] {
		case gifTrailer:
			if frames == 0 {
				return 0, fmt.Errorf("%w: the GIF has no image; export the image again", ErrImageHeader)
			}
			return frames, nil
		case gifExtension:
			pos, err = gifSkipSubBlocks(data, pos+2)
		case gifImageDescriptor:
			pos, err = gifSkipImage(data, pos+1, screen)
			frames++
		default:
			return 0, fmt.Errorf("%w: the GIF has unknown block %#x at byte %d; export the image again", ErrImageHeader, data[pos], pos)
		}
		if err != nil {
			return 0, err
		}
	}
	return 0, errGIFTruncated
}

func gifSkipImage(data []byte, pos int, screen image.Point) (int, error) {
	if pos+9 > len(data) {
		return 0, errGIFTruncated
	}
	left, top := int(binary.LittleEndian.Uint16(data[pos:])), int(binary.LittleEndian.Uint16(data[pos+2:]))
	width, height := int(binary.LittleEndian.Uint16(data[pos+4:])), int(binary.LittleEndian.Uint16(data[pos+6:]))
	if left+width > screen.X || top+height > screen.Y {
		return 0, fmt.Errorf("%w: a GIF frame of %dx%d pixels at %d,%d lies outside the %dx%d screen; export the image again", ErrImageHeader, width, height, left, top, screen.X, screen.Y)
	}
	return gifSkipSubBlocks(data, pos+9+gifColorTableBytes(data[pos+8])+1)
}

func gifSkipSubBlocks(data []byte, pos int) (int, error) {
	for pos < len(data) {
		size := int(data[pos])
		pos += 1 + size
		if size == 0 {
			return pos, nil
		}
	}
	return 0, errGIFTruncated
}

func DecodeImage(data []byte) (*image.NRGBA, ImageHeader, error) {
	header, err := InspectImage(data)
	if err != nil {
		return nil, ImageHeader{}, err
	}
	img, err := decodeImage(data, header)
	if err != nil {
		return nil, ImageHeader{}, err
	}
	return img, header, nil
}

func decodeImage(data []byte, header ImageHeader) (*image.NRGBA, error) {
	if buffer := imageProduct(imageProduct(int64(header.Width), int64(header.Height)), rgbaPixelBytes); buffer > ImagePixelByteLimit {
		return nil, fmt.Errorf("%w: the %d-byte display buffer exceeds the %d-byte limit; resize the image or save it with 8 bits per channel", ErrImageLimit, buffer, ImagePixelByteLimit)
	}
	decoded, err := decodeCheckedFrame(data, header.Format)
	if err != nil {
		return nil, err
	}
	orientation := imageOrientations[header.Orientation]
	if straight, ok := decoded.(*image.NRGBA); ok && orientation == (imageOrientation{}) {
		return straight, nil
	}
	return orientPixels(imageSource(decoded), header.Width, header.Height, orientation), nil
}

func decodeCheckedFrame(data []byte, format ImageFormat) (image.Image, error) {
	decoded, err := decodeFirstFrame(format, data)
	if err != nil {
		return nil, fmt.Errorf("%w: the %s data is damaged or uses an unsupported feature (%w); export the image again", ErrImageDecode, format, err)
	}
	return decoded, nil
}

func decodeFirstFrame(format ImageFormat, data []byte) (image.Image, error) {
	source := bytes.NewReader(data)
	switch format {
	case ImagePNG:
		return png.Decode(source)
	case ImageJPEG:
		return jpeg.Decode(source)
	case ImageGIF:
		return gif.Decode(source)
	}
	return nil, fmt.Errorf("%w: format %d has no decoder", ErrImageFormat, format)
}

func imageSource(m image.Image) func(x, y int) color.NRGBA {
	switch m := m.(type) {
	case *image.NRGBA:
		return m.NRGBAAt
	case *image.Paletted:
		return palettedSource(m)
	case *image.YCbCr:
		return straightSource(m.YCbCrAt)
	case *image.Gray:
		return straightSource(m.GrayAt)
	case *image.RGBA:
		return straightSource(m.RGBAAt)
	}
	return straightSource(m.At)
}

func straightSource[C color.Color](at func(x, y int) C) func(x, y int) color.NRGBA {
	return func(x, y int) color.NRGBA {
		r, g, b, a := at(x, y).RGBA()
		return straightColor(r, g, b, a)
	}
}

func straightColor(r, g, b, a uint32) color.NRGBA {
	switch a {
	case 0:
		return color.NRGBA{}
	case 0xFFFF:
		return color.NRGBA{R: uint8(r >> 8), G: uint8(g >> 8), B: uint8(b >> 8), A: 0xFF} //nolint:gosec // color.Color channels are at most 0xFFFF, so each high byte fits
	}
	return color.NRGBA{R: uint8(r * 0xFFFF / a >> 8), G: uint8(g * 0xFFFF / a >> 8), B: uint8(b * 0xFFFF / a >> 8), A: uint8(a >> 8)} //nolint:gosec // premultiplied channels never exceed alpha, so each quotient is at most 0xFFFF
}

func palettedSource(m *image.Paletted) func(x, y int) color.NRGBA {
	var palette [256]color.NRGBA
	for i, entry := range m.Palette[:min(len(m.Palette), len(palette))] {
		palette[i] = color.NRGBAModel.Convert(entry).(color.NRGBA)
	}
	return func(x, y int) color.NRGBA {
		if !image.Pt(x, y).In(m.Rect) {
			return color.NRGBA{}
		}
		return palette[m.Pix[m.PixOffset(x, y)]]
	}
}

func ImagePreview(src *image.NRGBA, maxSide int) *image.NRGBA {
	width, height := src.Rect.Dx(), src.Rect.Dy()
	previewWidth, previewHeight := imagePreviewSize(width, height, max(maxSide, 1))
	preview := image.NewNRGBA(image.Rect(0, 0, previewWidth, previewHeight))
	if previewWidth == width && previewHeight == height {
		copyNRGBARows(preview, src, src.Rect)
		return preview
	}
	columns := previewAxis{source: width, preview: previewWidth}
	rows := previewAxis{source: height, preview: previewHeight}
	sums := make([]previewSum, previewWidth)
	area := int64(width) * int64(height)
	for y := range previewHeight {
		clear(sums)
		first, last := rows.span(y)
		for sourceY := first; sourceY <= last; sourceY++ {
			columns.accumulate(sums, src.Pix[src.PixOffset(src.Rect.Min.X, src.Rect.Min.Y+sourceY):], rows.weight(sourceY, y))
		}
		for x, sum := range sums {
			preview.SetNRGBA(x, y, sum.color(area))
		}
	}
	return preview
}

func imagePreviewSize(width, height, maxSide int) (int, int) {
	longer := max(width, height)
	if longer <= maxSide {
		return width, height
	}
	return max(1, (width*maxSide+longer/2)/longer), max(1, (height*maxSide+longer/2)/longer)
}

type previewAxis struct{ source, preview int }

func (a previewAxis) span(o int) (int, int) {
	return o * a.source / a.preview, ((o+1)*a.source - 1) / a.preview
}

func (a previewAxis) weight(i, o int) int64 {
	return int64(min((i+1)*a.preview, (o+1)*a.source) - max(i*a.preview, o*a.source))
}

func (a previewAxis) accumulate(sums []previewSum, row []byte, rowWeight int64) {
	for x := range sums {
		first, last := a.span(x)
		for sourceX := first; sourceX <= last; sourceX++ {
			sums[x].add(row[rgbaPixelBytes*sourceX:], rowWeight*a.weight(sourceX, x))
		}
	}
}

type previewSum struct{ red, green, blue, alpha int64 }

func (s *previewSum) add(pixel []byte, weight int64) {
	alpha := int64(pixel[3]) * weight
	s.red += int64(pixel[0]) * alpha
	s.green += int64(pixel[1]) * alpha
	s.blue += int64(pixel[2]) * alpha
	s.alpha += alpha
}

func (s previewSum) color(area int64) color.NRGBA {
	if s.alpha == 0 {
		return color.NRGBA{}
	}
	half := s.alpha / 2
	return color.NRGBA{R: uint8((s.red + half) / s.alpha), G: uint8((s.green + half) / s.alpha), B: uint8((s.blue + half) / s.alpha), A: uint8((s.alpha + area/2) / area)} //nolint:gosec // alpha-weighted means of byte channels stay within 0 to 255
}

func ImageTile(src *image.NRGBA, r image.Rectangle) (*image.NRGBA, error) {
	width, height := imageSpan(r.Min.X, r.Max.X), imageSpan(r.Min.Y, r.Max.Y)
	if width > TileSideLimit || height > TileSideLimit {
		return nil, fmt.Errorf("%w: the tile request covers %dx%d pixels and a tile may cover at most %dx%d; request smaller tiles", ErrImageLimit, width, height, TileSideLimit, TileSideLimit)
	}
	area := r.Intersect(src.Rect)
	tile := image.NewNRGBA(area)
	copyNRGBARows(tile, src, area)
	return tile, nil
}

func imageSpan(low, high int) int {
	switch {
	case high <= low:
		return 0
	case low < 0 && high > math.MaxInt+low:
		return math.MaxInt
	}
	return high - low
}

func copyNRGBARows(dst, src *image.NRGBA, area image.Rectangle) {
	rowBytes := area.Dx() * rgbaPixelBytes
	for y := range area.Dy() {
		copy(dst.Pix[y*dst.Stride:][:rowBytes], src.Pix[src.PixOffset(area.Min.X, area.Min.Y+y):])
	}
}

func AdmitImages(existing []int64, candidates []ImageCandidate) ImageAdmission {
	const aggregateCeiling = ImageAggregateLimit + 1
	var used int64
	for _, size := range existing {
		used = min(used+min(max(size, 0), aggregateCeiling), aggregateCeiling)
	}
	count := len(existing)
	var admission ImageAdmission
	for _, candidate := range candidates {
		header, err := admitImage(count, used, candidate.Data)
		if err != nil {
			admission.Failures = append(admission.Failures, ImageFailure{Name: candidate.Name, Err: err})
			continue
		}
		admission.Accepted = append(admission.Accepted, candidate)
		admission.Headers = append(admission.Headers, header)
		count++
		used += int64(len(candidate.Data))
	}
	return admission
}

func admitImage(count int, used int64, data []byte) (ImageHeader, error) {
	if count >= ImageCountLimit {
		return ImageHeader{}, fmt.Errorf("%w: the message already has %d images and may have at most %d; remove an image before adding another", ErrImageLimit, count, ImageCountLimit)
	}
	header, err := InspectImage(data)
	if err != nil {
		return ImageHeader{}, err
	}
	if total := used + int64(len(data)); total > ImageAggregateLimit {
		return ImageHeader{}, fmt.Errorf("%w: the images would total %d bytes and a message may carry at most %d; remove an image or attach a smaller one", ErrImageLimit, total, ImageAggregateLimit)
	}
	return header, nil
}
