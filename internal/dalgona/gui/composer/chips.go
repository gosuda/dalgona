//go:build gui

package composer

import (
	"bytes"
	"encoding/base64"
	"fmt"
	"image"
	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	pasteChipLines   = 10
	imageLimit       = 10
	imageBytes       = 5 << 20
	imageTotalBytes  = 10 << 20
	inputBudgetBytes = 15 << 20
	imageSideLimit   = 16384
	imagePixelLimit  = 32_000_000
)

var messagePolicy = textinput.Policy{Lines: textinput.MultiLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: inputBudgetBytes}}

type refusal string

func (r refusal) Error() string { return string(r) }

const (
	errImagesRefused refusal = "this model does not accept images"
	errImageType     refusal = "unsupported image type; send PNG, JPEG or GIF"
	errImageTooLarge refusal = "Image is larger than 5 MiB."
	errImageCount    refusal = "At most 10 images per message."
	errImageTotal    refusal = "The images together exceed 10 MiB; remove an image."
	errImageSize     refusal = "The image is larger than 16384 pixels on a side or 32 million pixels; send a smaller image."
	errInputBudget   refusal = "The message and attachments exceed 15 MiB; remove an attachment or shorten the text."
)

type imageFormat struct {
	mime  string
	name  string
	magic []string
}

var imageFormats = [...]imageFormat{
	{mime: "image/png", name: "PNG", magic: []string{"\x89PNG\r\n\x1a\n"}},
	{mime: "image/jpeg", name: "JPEG", magic: []string{"\xff\xd8\xff"}},
	{mime: "image/gif", name: "GIF", magic: []string{"GIF87a", "GIF89a"}},
}

func sniffImage(data []byte) (imageFormat, bool) {
	for _, f := range imageFormats {
		for _, m := range f.magic {
			if bytes.HasPrefix(data, []byte(m)) {
				return f, true
			}
		}
	}
	return imageFormat{}, false
}

type pastedText struct {
	text  string
	lines int
}

func normalizePaste(candidate string) (pastedText, error) {
	adapted, err := textinput.Draft{}.Replace(messagePolicy, candidate)
	if err != nil {
		return pastedText{}, err
	}
	return pastedText{text: adapted.Text, lines: lineCount(adapted.Text)}, nil
}

func lineCount(text string) int {
	if text == "" {
		return 0
	}
	return strings.Count(strings.TrimSuffix(text, "\n"), "\n") + 1
}

func pasteChip(id string, p pastedText) state.Attachment {
	return state.Attachment{ID: id, Kind: state.AttachmentText, Name: fmt.Sprintf("Pasted text · %d lines", p.lines), Text: p.text}
}

type imageFile struct {
	name string
	data []byte
}

func imageChip(id string, f imageFile) (state.Attachment, error) {
	format, ok := sniffImage(f.data)
	if !ok {
		return state.Attachment{}, errImageType
	}
	if len(f.data) > imageBytes {
		return state.Attachment{}, errImageTooLarge
	}
	cfg, _, err := image.DecodeConfig(bytes.NewReader(f.data))
	if err != nil {
		return state.Attachment{}, fmt.Errorf("the %s image cannot be read: %w", format.name, err)
	}
	if cfg.Width > imageSideLimit || cfg.Height > imageSideLimit || cfg.Width*cfg.Height > imagePixelLimit {
		return state.Attachment{}, errImageSize
	}
	url := "data:" + format.mime + ";base64," + base64.StdEncoding.EncodeToString(f.data)
	return state.Attachment{ID: id, Kind: state.AttachmentImage, Name: f.name, URL: url}, nil
}

func admitImage(chips []state.Attachment, next state.Attachment) error {
	count, total := 0, imageSize(next)
	for _, c := range chips {
		if c.Kind == state.AttachmentImage {
			count++
			total += imageSize(c)
		}
	}
	switch {
	case count >= imageLimit:
		return errImageCount
	case total > imageTotalBytes:
		return errImageTotal
	}
	return nil
}

func imageSize(a state.Attachment) int {
	_, payload, ok := strings.Cut(a.URL, ";base64,")
	if !ok {
		return 0
	}
	n := base64.StdEncoding.DecodedLen(len(payload))
	return n - strings.Count(payload[max(0, len(payload)-2):], "=")
}

func hasImages(chips []state.Attachment) bool {
	for _, c := range chips {
		if c.Kind == state.AttachmentImage {
			return true
		}
	}
	return false
}

func inputBytes(typed string, chips []state.Attachment) int {
	n, items := len("[]"), 0
	add := func(size int) {
		if items > 0 {
			n++
		}
		n += size
		items++
	}
	if typed != "" {
		add(len(`{"type":"text","text":}`) + quotedLen(typed))
	}
	for _, c := range chips {
		if c.Kind == state.AttachmentText {
			add(len(`{"type":"text","text":}`) + quotedLen(c.Text))
		}
	}
	for _, c := range chips {
		if c.Kind == state.AttachmentImage {
			add(len(`{"type":"image","url":}`) + quotedLen(c.URL) + namedLen(c.Name))
		}
	}
	return n
}

func quotedLen(s string) int {
	n := 2
	for i := 0; i < len(s); i++ {
		n += escapedWidth(s[i])
	}
	return n
}

func namedLen(name string) int {
	if name == "" {
		return 0
	}
	return len(`,"name":`) + quotedLen(name)
}

func escapedWidth(c byte) int {
	switch c {
	case '"', '\\', '\b', '\f', '\n', '\r', '\t':
		return 2
	}
	if c < 0x20 {
		return len(`\u0000`)
	}
	return 1
}
