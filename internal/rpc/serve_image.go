package rpc

import (
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
)

const maxImagePathBytes = 4096

func imageTooLarge(index int, size int64) error {
	return invalidParams(fmt.Sprintf("image %d is %d bytes; the limit is %d bytes (5 MiB)", index, size, event.MaxImageBytes))
}

func imageFromURL(raw string, admit func(int64) error) (agent.Image, error) {
	rest, hasPrefix := strings.CutPrefix(raw, "data:")
	header, payload, hasComma := strings.Cut(rest, ",")
	declared, isBase64 := strings.CutSuffix(header, ";base64")
	if !hasPrefix || !hasComma || !isBase64 {
		return agent.Image{}, invalidParams("image url must be a data: URL with base64 data")
	}
	if err := admit(decodedSize(payload)); err != nil {
		return agent.Image{}, err
	}
	data, err := base64.StdEncoding.DecodeString(payload)
	if err != nil {
		return agent.Image{}, invalidParams("image data is not valid base64")
	}
	mime, err := agent.ImageMIME(data)
	if err != nil {
		return agent.Image{}, invalidParams(err.Error())
	}
	if mime != declared {
		return agent.Image{}, invalidParams(fmt.Sprintf("image type %s does not match the data URL type %s", mime, declared))
	}
	return agent.Image{Mime: mime, Data: data}, nil
}

func decodedSize(payload string) int64 {
	padding := len(payload) - len(strings.TrimRight(payload, "="))
	return int64(base64.StdEncoding.DecodedLen(len(payload)) - padding)
}

func imageFromPath(path string, admit func(int64) error) (agent.Image, error) {
	if !filepath.IsAbs(path) {
		return agent.Image{}, invalidParams("image path must be absolute: " + path)
	}
	if len(path) > maxImagePathBytes {
		return agent.Image{}, invalidParams("image path is longer than 4096 bytes")
	}
	info, err := os.Stat(path)
	if err != nil {
		return agent.Image{}, imageReadError(path, err)
	}
	if !info.Mode().IsRegular() {
		return agent.Image{}, invalidParams("image path must be a regular file: " + path)
	}
	if err = admit(info.Size()); err != nil {
		return agent.Image{}, err
	}
	data, err := readImageFile(path)
	if err != nil {
		return agent.Image{}, err
	}
	if err = admit(int64(len(data))); err != nil {
		return agent.Image{}, err
	}
	mime, err := agent.ImageMIME(data)
	if err != nil {
		return agent.Image{}, invalidParams(err.Error())
	}
	return agent.Image{Path: filepath.Clean(path), Mime: mime, Data: data}, nil
}

func readImageFile(path string) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, imageReadError(path, err)
	}
	defer func() { _ = file.Close() }()
	data, err := io.ReadAll(io.LimitReader(file, event.MaxImageBytes+1))
	if err != nil {
		return nil, imageReadError(path, err)
	}
	return data, nil
}

func imageReadError(path string, err error) error {
	if errors.Is(err, fs.ErrNotExist) {
		return invalidParams("image file not found: " + path)
	}
	return internalError(fmt.Errorf("cannot read image %s: %w", path, err))
}

func (p *turnParams) takeImage(load func(admit func(int64) error) (agent.Image, error)) {
	if p.imageErr != nil {
		return
	}
	if len(p.images) == event.MaxImages {
		p.imageErr = invalidParams(fmt.Sprintf("a request takes at most %d images, got %d", event.MaxImages, event.MaxImages+1))
		return
	}
	image, err := load(p.admit)
	if err != nil {
		p.imageErr = err
		return
	}
	p.images = append(p.images, image)
	p.imageBytes += int64(len(image.Data))
}

func (p *turnParams) admit(size int64) error {
	if size > event.MaxImageBytes {
		return imageTooLarge(len(p.images)+1, size)
	}
	if total := p.imageBytes + size; total > agent.MaxRequestImageBytes {
		return invalidParams(fmt.Sprintf("images total %d bytes; the limit is %d bytes (10 MiB) per request", total, agent.MaxRequestImageBytes))
	}
	return nil
}

func (p *turnParams) checkedImages() ([]agent.Image, error) {
	if p.imageErr != nil {
		return nil, p.imageErr
	}
	if err := agent.CheckImages(p.images); err != nil {
		return nil, invalidParams(err.Error())
	}
	return p.images, nil
}
