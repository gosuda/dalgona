package rpcserver

import (
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const maxImagePathBytes = 4096

func imageTooLarge(index int, size int64) error {
	return invalidParams(fmt.Sprintf("image %d is %d bytes; the limit is %d bytes (5 MiB)", index, size, event.MaxImageBytes))
}

func imageFromURL(raw, name, alt string, admit func(int64) error) (agent.Image, error) {
	if err := checkImageMeta(name, alt); err != nil {
		return agent.Image{}, err
	}
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
	return agent.Image{Mime: mime, Data: data, Name: name, AltText: alt}, nil
}

func decodedSize(payload string) int64 {
	padding := len(payload) - len(strings.TrimRight(payload, "="))
	return int64(base64.StdEncoding.DecodedLen(len(payload)) - padding)
}

func checkImagePath(path string) error {
	if !filepath.IsAbs(path) {
		return invalidParams("image path must be absolute: " + path)
	}
	if len(path) > maxImagePathBytes {
		return invalidParams("image path is longer than 4096 bytes")
	}
	return nil
}

func statImageFile(path string) (os.FileInfo, error) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, imageReadError(path, err)
	}
	if !info.Mode().IsRegular() {
		return nil, invalidParams("image path must be a regular file: " + path)
	}
	return info, nil
}

func loadAdmittedImage(path string, admit func(int64) error) ([]byte, error) {
	info, err := statImageFile(path)
	if err != nil {
		return nil, err
	}
	if err = admit(info.Size()); err != nil {
		return nil, err
	}
	data, err := readImageFile(path)
	if err != nil {
		return nil, err
	}
	if err = admit(int64(len(data))); err != nil {
		return nil, err
	}
	return data, nil
}

func imageFromPath(path, name, alt string, admit func(int64) error) (agent.Image, error) {
	if err := checkImagePath(path); err != nil {
		return agent.Image{}, err
	}
	if err := checkImageMeta(name, alt); err != nil {
		return agent.Image{}, err
	}
	data, err := loadAdmittedImage(path, admit)
	if err != nil {
		return agent.Image{}, err
	}
	mime, err := agent.ImageMIME(data)
	if err != nil {
		return agent.Image{}, invalidParams(err.Error())
	}
	return agent.Image{Path: filepath.Clean(path), Mime: mime, Data: data, Name: name, AltText: alt}, nil
}

func checkImageMeta(name, alt string) error {
	if len(name) > event.MaxImageNameBytes {
		return invalidParams(fmt.Sprintf("image name exceeds %d bytes", event.MaxImageNameBytes))
	}
	if len(alt) > event.MaxImageAltBytes {
		return invalidParams(fmt.Sprintf("image alt_text exceeds %d bytes", event.MaxImageAltBytes))
	}
	return nil
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

type imageLoad func(admit func(int64) error) (agent.Image, error)

func (p *turnParams) queueImage(load imageLoad) {
	if p.imageErr != nil {
		return
	}
	if len(p.loads) == event.MaxImages {
		p.imageErr = invalidParams(fmt.Sprintf("a request takes at most %d images, got %d", event.MaxImages, event.MaxImages+1))
		return
	}
	p.loads = append(p.loads, load)
}

func (p *turnParams) loadImages() {
	for _, load := range p.loads {
		if p.imageErr != nil {
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

const blobURLPrefix = "dalgona-blob:"

func itemImages(images []event.Image) []rpc.ItemImage {
	out := make([]rpc.ItemImage, len(images))
	for index, image := range images {
		out[index] = rpc.ItemImage{Type: "image", URL: blobURLPrefix + image.Blob.Blob, Mime: image.Mime, Bytes: image.Blob.Bytes, Path: image.Path, Name: image.Name, AltText: image.AltText}
	}
	return out
}
