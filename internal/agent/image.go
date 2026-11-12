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
