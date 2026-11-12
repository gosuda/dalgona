package agent

import (
	"fmt"
	"net/http"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const MaxRequestImageBytes = 10 << 20

type Image struct {
	Path, Mime string
	Data       []byte
}

func ImageMIME(data []byte) (string, error) {
	mime := http.DetectContentType(data)
	switch mime {
	case "image/png", "image/jpeg", "image/gif":
		return mime, nil
	}
	return "", fmt.Errorf("unsupported image type %s; send PNG, JPEG or GIF", mime)
}

func CheckImages(images []Image) error {
	if len(images) > event.MaxImages {
		return fmt.Errorf("a request takes at most %d images, got %d", event.MaxImages, len(images))
	}
	var total int64
	for i, image := range images {
		size := int64(len(image.Data))
		if size > event.MaxImageBytes {
			return fmt.Errorf("image %d is %d bytes; the limit is %d bytes (5 MiB)", i+1, size, event.MaxImageBytes)
		}
		total += size
	}
	if total > MaxRequestImageBytes {
		return fmt.Errorf("images total %d bytes; the limit is %d bytes (10 MiB) per request", total, MaxRequestImageBytes)
	}
	return nil
}

func (t *Thread) prepareImages(images []Image) ([]event.Image, error) {
	if err := CheckImages(images); err != nil {
		return nil, err
	}
	var out []event.Image
	for _, image := range images {
		mime, err := ImageMIME(image.Data)
		if err != nil {
			return nil, err
		}
		if mime != image.Mime {
			return nil, fmt.Errorf("image type %s does not match its declared type %s", mime, image.Mime)
		}
		path := ""
		if image.Path != "" {
			if path, err = t.host.mask(image.Path); err != nil {
				return nil, err
			}
		}
		blob, err := t.PutBlob(image.Data)
		if err != nil {
			return nil, err
		}
		out = append(out, event.Image{Mime: image.Mime, Path: path, Blob: blob})
	}
	return out, nil
}
