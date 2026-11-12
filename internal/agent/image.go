package agent

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"net/http"
	"sync"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
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

type verifiedImages struct {
	mu    sync.Mutex
	data  map[string]verifiedImage
	loads int
}

type verifiedImage struct {
	mime string
	data []byte
}

func (v *verifiedImages) lookup(sha string) (verifiedImage, bool) {
	v.mu.Lock()
	defer v.mu.Unlock()
	image, ok := v.data[sha]
	return image, ok
}

func (v *verifiedImages) store(sha string, image verifiedImage) {
	v.mu.Lock()
	defer v.mu.Unlock()
	if v.data == nil {
		v.data = make(map[string]verifiedImage)
	}
	v.data[sha] = image
	v.loads++
}

func (t *Thread) imageParts(images []event.Image) ([]model.Image, error) {
	out := make([]model.Image, 0, len(images))
	for _, image := range images {
		data, err := t.verifiedImage(image)
		if err != nil {
			return nil, err
		}
		out = append(out, model.Image{Mime: image.Mime, Data: data})
	}
	return out, nil
}

func (t *Thread) verifiedImage(image event.Image) ([]byte, error) {
	sha := image.Blob.Blob
	if cached, ok := t.images.lookup(sha); ok {
		if err := checkImageRecord(image, cached.mime, len(cached.data)); err != nil {
			return nil, err
		}
		return cached.data, nil
	}
	data, err := t.host.journal.Blob(sha)
	if err != nil {
		return nil, fmt.Errorf("agent: load image blob %s: %w", sha, err)
	}
	if err := checkImageBlob(image, data); err != nil {
		return nil, err
	}
	t.images.store(sha, verifiedImage{mime: image.Mime, data: data})
	return data, nil
}

func checkImageBlob(image event.Image, data []byte) error {
	sum := sha256.Sum256(data)
	mime, err := ImageMIME(data)
	if hex.EncodeToString(sum[:]) != image.Blob.Blob || err != nil {
		return blobMismatch(image)
	}
	return checkImageRecord(image, mime, len(data))
}

func checkImageRecord(image event.Image, mime string, size int) error {
	if mime != image.Mime || int64(size) != image.Blob.Bytes {
		return blobMismatch(image)
	}
	return nil
}

func blobMismatch(image event.Image) error {
	return fmt.Errorf("agent: image blob %s does not match its journal record", image.Blob.Blob)
}

func sendsImages(registry *model.Registry, provider string) bool {
	adapter, ok := registry.Provider(provider)
	if !ok {
		return false
	}
	sender, ok := adapter.(model.ImageSender)
	return ok && sender.SendsImages()
}
