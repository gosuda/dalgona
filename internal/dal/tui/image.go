package tui

import (
	"bytes"
	"context"
	"encoding/base64"
	"fmt"
	"image"
	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
)

const (
	imageLimit        = 5 << 20
	imageRequestLimit = 10 << 20
	imageCountLimit   = 10
	clipboardTimeout  = 5 * time.Second
)

type imageMsg struct {
	text          string
	mime, data    string
	width, height int
	err           error
}

func imageError(format string, args ...any) error {
	return fmt.Errorf(format, args...)
}

func imagePath(text string) (string, bool) {
	if strings.Contains(text, "\n") {
		return "", false
	}
	path := unquotePaste(text)
	switch strings.ToLower(filepath.Ext(path)) {
	case ".png", ".jpg", ".jpeg", ".gif", ".webp":
		return path, true
	}
	return "", false
}

func unquotePaste(text string) string {
	path := text
	if n := len(path); n >= 2 && (path[0] == '\'' || path[0] == '"') && path[n-1] == path[0] {
		path = path[1 : n-1]
	}
	return strings.TrimPrefix(path, "file://")
}

func unescapePaste(path string) string {
	var out strings.Builder
	for i := 0; i < len(path); i++ {
		if path[i] == '\\' && i+1 < len(path) && strings.IndexByte(" '\"()", path[i+1]) >= 0 {
			i++
		}
		out.WriteByte(path[i])
	}
	return out.String()
}

func resolvePaste(raw string) string {
	if _, err := os.Stat(raw); err == nil {
		return raw
	}
	return unescapePaste(raw)
}

func pastedImage(text, raw string) tea.Cmd {
	return func() tea.Msg {
		path := resolvePaste(raw)
		info, err := os.Stat(path)
		if err != nil || !info.Mode().IsRegular() {
			return nil
		}
		name := filepath.Base(path)
		if info.Size() > imageLimit {
			return imageMsg{text: text, err: imageError("Image %s is %d bytes, over the 5 MiB limit", name, info.Size())}
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return imageMsg{text: text, err: imageError("cannot read image %s: %v", name, err)}
		}
		msg := decodeImage(name, data)
		msg.text = text
		return msg
	}
}

func decodeImage(name string, data []byte) imageMsg {
	if len(data) > imageLimit {
		return imageMsg{err: imageError("Image %s is %d bytes, over the 5 MiB limit", name, len(data))}
	}
	mime := http.DetectContentType(data)
	switch mime {
	case "image/png", "image/jpeg", "image/gif":
	default:
		return imageMsg{err: imageError("unsupported image type %s; send PNG, JPEG or GIF", mime)}
	}
	config, _, err := image.DecodeConfig(bytes.NewReader(data))
	if err != nil {
		return imageMsg{err: imageError("cannot read image %s: %v", name, err)}
	}
	return imageMsg{mime: mime, data: base64.StdEncoding.EncodeToString(data), width: config.Width, height: config.Height}
}

func decodedBytes(encoded string) int {
	padding := len(encoded) - len(strings.TrimRight(encoded, "="))
	return base64.StdEncoding.DecodedLen(len(encoded)) - padding
}

func clipboardTool(goos string, getenv func(string) string, lookPath func(string) (string, error)) ([]string, bool) {
	switch goos {
	case "darwin":
		_, err := lookPath("pngpaste")
		return []string{"pngpaste", "-"}, err == nil
	case "windows":
		script := "$i = Get-Clipboard -Format Image; if ($i) { $m = New-Object IO.MemoryStream; $i.Save($m, [Drawing.Imaging.ImageFormat]::Png); [Convert]::ToBase64String($m.ToArray()) }"
		return []string{"powershell", "-NoProfile", "-Command", script}, true
	}
	if getenv("WAYLAND_DISPLAY") != "" {
		if _, err := lookPath("wl-paste"); err == nil {
			return []string{"wl-paste", "--type", "image/png"}, true
		}
	}
	if getenv("DISPLAY") != "" {
		if _, err := lookPath("xclip"); err == nil {
			return []string{"xclip", "-selection", "clipboard", "-t", "image/png", "-o"}, true
		}
	}
	return nil, false
}

func clipboardBinary(argv []string) tea.Cmd {
	return clipboardRead(argv, func(out []byte) ([]byte, error) { return out, nil })
}

func clipboardBase64(argv []string) tea.Cmd {
	return clipboardRead(argv, func(out []byte) ([]byte, error) {
		return base64.StdEncoding.DecodeString(strings.TrimSpace(string(out)))
	})
}

func clipboardRead(argv []string, decode func([]byte) ([]byte, error)) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), clipboardTimeout)
		defer cancel()
		out, err := exec.CommandContext(ctx, argv[0], argv[1:]...).Output()
		if err != nil {
			return imageMsg{err: imageError("cannot read image clipboard: %v", err)}
		}
		data, err := decode(out)
		if err != nil {
			return imageMsg{err: imageError("cannot read image clipboard: %v", err)}
		}
		if len(data) == 0 {
			return imageMsg{err: imageError("cannot read image clipboard: the clipboard holds no image")}
		}
		return decodeImage("clipboard", data)
	}
}
