package codexapp

import (
	"bytes"
	"cmp"
	"fmt"
	"os"
	"path/filepath"
)

var linuxEnvironmentFile = fileMechanism{
	name:     "environment.d",
	path:     linuxEnvironmentPath,
	expected: func(exe string) string { return "CODEX_CLI_PATH=" + exe },
	content: func(_ Env, exe string) ([]byte, error) {
		return []byte("CODEX_CLI_PATH=" + exe + "\n"), nil
	},
}

var linuxDesktopFile = fileMechanism{
	name:     "desktop",
	path:     linuxDesktopPath,
	enabled:  linuxDesktopEnabled,
	expected: func(exe string) string { return "env CODEX_CLI_PATH=" + exe },
	content:  linuxDesktopContent,
}

func linuxEnvironmentPath(env Env) string {
	return filepath.Join(userHome(env), ".config", "environment.d", "60-dalgona-codex.conf")
}

func linuxDesktopPath(env Env) string {
	return filepath.Join(userHome(env), ".local", "share", "applications", "chatgpt.desktop")
}

func desktopSource(env Env) string {
	return cmp.Or(env.DesktopFile, "/usr/share/applications/chatgpt.desktop")
}

func linuxDesktopEnabled(env Env) bool {
	_, err := os.Stat(desktopSource(env))
	return err == nil
}

func linuxDesktopContent(env Env, exe string) ([]byte, error) {
	data, err := os.ReadFile(desktopSource(env))
	if err != nil {
		return nil, fmt.Errorf("codexapp: read desktop file: %w", err)
	}
	return rewriteDesktop(data, exe), nil
}

func rewriteDesktop(src []byte, exe string) []byte {
	lines := bytes.SplitAfter(src, []byte("\n"))
	for i, line := range lines {
		if rewritten, ok := rewriteExecLine(line, exe); ok {
			lines[i] = rewritten
			break
		}
	}
	return bytes.Join(lines, nil)
}

func rewriteExecLine(line []byte, exe string) ([]byte, bool) {
	body := bytes.TrimSuffix(line, []byte("\n"))
	body = bytes.TrimSuffix(body, []byte("\r"))
	ending := line[len(body):]
	trimmed := bytes.TrimSpace(body)
	if !bytes.HasPrefix(trimmed, []byte("Exec=")) {
		return nil, false
	}
	command := bytes.TrimPrefix(trimmed, []byte("Exec="))
	prefix := []byte("Exec=env CODEX_CLI_PATH=" + exe + " ")
	rewritten := make([]byte, 0, len(prefix)+len(command)+len(ending))
	rewritten = append(rewritten, prefix...)
	rewritten = append(rewritten, command...)
	rewritten = append(rewritten, ending...)
	return rewritten, true
}
