package codexapp

import (
	"errors"
	"fmt"
	"os/exec"
	"path/filepath"
	"strings"
)

var launchAgentFile = fileMechanism{
	name:     "launch-agent",
	path:     launchAgentPath,
	expected: xmlEscape,
	content: func(_ Env, exe string) ([]byte, error) {
		return []byte(plist(exe)), nil
	},
}

var launchctlEnvironment = environmentMechanism{
	name:    "launchctl",
	current: launchctlValue,
	set:     setLaunchctl,
	restore: restoreLaunchctl,
}

func launchAgentPath(env Env) string {
	return filepath.Join(userHome(env), "Library", "LaunchAgents", "dev.dalgona.codex-cli-path.plist")
}

func launchctlValue(env Env) (string, bool, error) {
	output, err := commandOutput(env, "launchctl", "getenv", "CODEX_CLI_PATH")
	if err == nil {
		value := strings.TrimSpace(string(output))
		return value, value != "", nil
	}
	exitErr, isExitError := errors.AsType[*exec.ExitError](err)
	if errors.Is(err, exec.ErrNotFound) || isExitError && exitErr.ExitCode() == 1 {
		return "", false, nil
	}
	return "", false, fmt.Errorf("launchctl getenv: %w", err)
}

func setLaunchctl(env Env, exe string) error {
	if err := runCommand(env, "launchctl", "setenv", "CODEX_CLI_PATH", exe); err != nil {
		return fmt.Errorf("launchctl setenv: %w", err)
	}
	return nil
}

func restoreLaunchctl(env Env, value string, found bool) error {
	if found {
		if err := runCommand(env, "launchctl", "setenv", "CODEX_CLI_PATH", value); err != nil {
			return fmt.Errorf("launchctl setenv: %w", err)
		}
		return nil
	}
	if err := runCommand(env, "launchctl", "unsetenv", "CODEX_CLI_PATH"); err != nil {
		return fmt.Errorf("launchctl unsetenv: %w", err)
	}
	return nil
}

func plist(exe string) string {
	return "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>dev.dalgona.codex-cli-path</string><key>ProgramArguments</key><array><string>/bin/launchctl</string><string>setenv</string><string>CODEX_CLI_PATH</string><string>" + xmlEscape(exe) + "</string></array><key>RunAtLoad</key><true/></dict></plist>\n"
}

func xmlEscape(value string) string {
	value = strings.ReplaceAll(value, "&", "&amp;")
	value = strings.ReplaceAll(value, "<", "&lt;")
	value = strings.ReplaceAll(value, ">", "&gt;")
	value = strings.ReplaceAll(value, "\"", "&quot;")
	return strings.ReplaceAll(value, "'", "&apos;")
}
