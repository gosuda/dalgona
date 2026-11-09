package codexapp

import (
	"bytes"
	"cmp"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
)

var vscodeFile = fileMechanism{
	name:     "vscode",
	path:     vscodeFilePath,
	expected: strconv.Quote,
	content:  vscodeContent,
}

func vscodeFilePath(env Env) string {
	return vscodePath(env, userHome(env))
}

func vscodePath(env Env, home string) string {
	if env.VSCodeSettings != "" {
		return env.VSCodeSettings
	}
	switch targetOS(env) {
	case "darwin":
		return filepath.Join(home, "Library", "Application Support", "Code", "User", "settings.json")
	case "windows":
		base := cmp.Or(env.APPDATA, os.Getenv("APPDATA"))
		return filepath.Join(base, "Code", "User", "settings.json")
	default:
		return filepath.Join(home, ".config", "Code", "User", "settings.json")
	}
}

func vscodeContent(env Env, exe string) ([]byte, error) {
	path := vscodeFilePath(env)
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		data = []byte("{}\n")
	} else if err != nil {
		return nil, fmt.Errorf("codexapp: read %s: %w", path, err)
	}
	return rewriteVSCode(data, exe), nil
}

func rewriteVSCode(src []byte, exe string) []byte {
	quoted := []byte(strconv.Quote(exe))
	start, end, ok := vscodeValueSpan(src)
	if ok {
		return replaceSpan(src, quoted, start, end)
	}
	return insertVSCodeValue(src, quoted)
}

func vscodeValueSpan(src []byte) (int, int, bool) {
	key := []byte("\"chatgpt.cliExecutable\"")
	keyAt := bytes.Index(src, key)
	if keyAt < 0 {
		return 0, 0, false
	}
	keyEnd := keyAt + len(key)
	colon := bytes.IndexByte(src[keyEnd:], ':')
	if colon < 0 {
		return 0, 0, false
	}
	start := keyEnd + colon + 1
	for start < len(src) && isJSONSpace(src[start]) {
		start++
	}
	if start >= len(src) || src[start] != '"' {
		return 0, 0, false
	}
	end := quotedEnd(src, start)
	return start, end, end >= 0
}

func isJSONSpace(value byte) bool {
	return value == ' ' || value == '\t' || value == '\r' || value == '\n'
}

func quotedEnd(text []byte, start int) int {
	escaped := false
	for i := start + 1; i < len(text); i++ {
		if text[i] == '\\' && !escaped {
			escaped = true
			continue
		}
		if text[i] == '"' && !escaped {
			return i
		}
		escaped = false
	}
	return -1
}

func replaceSpan(src, value []byte, start, end int) []byte {
	out := make([]byte, 0, len(src)-(end-start+1)+len(value))
	out = append(out, src[:start]...)
	out = append(out, value...)
	out = append(out, src[end+1:]...)
	return out
}

func insertVSCodeValue(src, quoted []byte) []byte {
	brace := bytes.IndexByte(src, '{')
	if brace < 0 {
		return src
	}
	rest := src[brace+1:]
	comma := []byte(nil)
	trimmed := bytes.TrimLeft(rest, " \t\r\n")
	if len(trimmed) > 0 && trimmed[0] != '}' {
		comma = []byte(",")
	}
	prefix := []byte("\n  \"chatgpt.cliExecutable\": ")
	out := make([]byte, 0, len(src)+len(prefix)+len(quoted)+len(comma))
	out = append(out, src[:brace+1]...)
	out = append(out, prefix...)
	out = append(out, quoted...)
	out = append(out, comma...)
	out = append(out, rest...)
	return out
}
