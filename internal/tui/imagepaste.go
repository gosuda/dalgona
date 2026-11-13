package tui

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"slices"

	tea "charm.land/bubbletea/v2"
)

func (m *model) imageRefusal() string {
	if !slices.Contains(m.thread.inputModalities[m.thread.modelName], "image") {
		return "this model does not accept images"
	}
	if len(m.input.composer.images()) >= imageCountLimit {
		return "at most 10 images per prompt"
	}
	return ""
}

func (m *model) imagePasteCmd(text string) tea.Cmd {
	path, ok := imagePath(text)
	if !ok {
		return nil
	}
	if refusal := m.imageRefusal(); refusal != "" {
		m.noticef("%s", refusal)
		return nil
	}
	return pastedImage(text, path)
}

func (m *model) clipboardImage() tea.Cmd {
	if m.textMode() != inputPrompt {
		return nil
	}
	if refusal := m.imageRefusal(); refusal != "" {
		m.noticef("%s", refusal)
		return nil
	}
	argv, ok := clipboardTool(runtime.GOOS, os.Getenv, exec.LookPath)
	if !ok {
		m.noticef("image paste needs wl-paste, xclip or pngpaste")
		return nil
	}
	if runtime.GOOS == "windows" {
		return clipboardBase64(argv)
	}
	return clipboardBinary(argv)
}

func (m *model) applyImage(msg imageMsg) {
	if msg.err != nil {
		m.noticef("%v", msg.err)
		return
	}
	if refusal := m.imageRefusal(); refusal != "" {
		m.noticef("%s", refusal)
		return
	}
	size := decodedBytes(msg.data)
	if total := m.input.composer.imageBytes() + size; total > imageRequestLimit {
		m.noticef("images total %d bytes; the limit is 10485760 bytes (10 MiB) per request", total)
		return
	}
	ch := chip{
		image: "data:" + msg.mime + ";base64," + msg.data,
		size:  fmt.Sprintf("%dx%d", msg.width, msg.height),
		bytes: size,
	}
	if !m.input.composer.attach(msg.text, ch) {
		m.input.composer.attach("", ch)
	}
}
