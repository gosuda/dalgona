package client

import (
	"context"
	"crypto/sha256"
	"debug/buildinfo"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
)

type SSHTarget struct {
	Destination string
	Artifact    string
}

type sshPlatform struct {
	os        string
	arch      string
	installed bool
}

func (r *Remotes) OnboardSSH(ctx context.Context, name string, target SSHTarget) (config.RemoteSpec, error) {
	ctx, cancel := context.WithTimeout(ctx, 3*time.Minute)
	defer cancel()
	return r.addMachine(ctx, name, func() (config.RemoteSpec, error) { return onboardSSH(ctx, target) })
}

func onboardSSH(ctx context.Context, target SSHTarget) (config.RemoteSpec, error) {
	if !config.ValidSSHDestination(target.Destination) {
		return config.RemoteSpec{}, errors.New("SSH needs a host alias or user@host; configure a custom port in OpenSSH")
	}
	platform, err := probeSSH(ctx, target.Destination)
	if err != nil {
		return config.RemoteSpec{}, err
	}
	spec := config.RemoteSpec{Kind: config.RemoteSSH, SSHDestination: target.Destination, SSHPlatform: platform.os}
	if platform.installed && target.Artifact == "" {
		return bindSSH(ctx, spec)
	}
	spec.SSHInstall, err = installSSH(ctx, target, platform)
	if err != nil {
		return config.RemoteSpec{}, err
	}
	return bindSSH(ctx, spec)
}

func bindSSH(ctx context.Context, spec config.RemoteSpec) (config.RemoteSpec, error) {
	stream, machineID, err := dialSSH(ctx, spec)
	if err != nil {
		return config.RemoteSpec{}, err
	}
	spec.MachineID = machineID
	return spec, stream.Close()
}

func probeSSH(ctx context.Context, destination string) (sshPlatform, error) {
	out, err := runSSH(ctx, destination, posixProbe, nil)
	if err == nil {
		return parseSSHPlatform(out)
	}
	if status, ok := errors.AsType[*exec.ExitError](err); !ok || status.ExitCode() == 255 || ctx.Err() != nil {
		return sshPlatform{}, err
	}
	out, err = runSSH(ctx, destination, powershell(windowsProbe), nil)
	if err != nil {
		return sshPlatform{}, fmt.Errorf("SSH requires a supported POSIX shell or Windows PowerShell before installation: %w", err)
	}
	return parseSSHPlatform(out)
}

func parseSSHPlatform(out string) (sshPlatform, error) {
	fields, err := sshProbeFields(out)
	if err != nil {
		return sshPlatform{}, err
	}
	var platform sshPlatform
	platform.os, err = normalizeSSHProbeOS(fields[1])
	if err != nil {
		return platform, err
	}
	platform.arch, err = normalizeSSHProbeArch(fields[2])
	if err != nil {
		return platform, err
	}
	platform.installed = fields[3] == "yes"
	return platform, nil
}

func sshProbeFields(out string) ([]string, error) {
	fields := strings.Fields(out)
	if len(fields) != 4 || fields[0] != "dalgona-ssh-v1" || fields[3] != "yes" && fields[3] != "no" {
		return nil, errors.New("unrecognized SSH platform probe; remove startup output and use a POSIX shell or Windows PowerShell")
	}
	return fields, nil
}

func normalizeSSHProbeOS(token string) (string, error) {
	switch token {
	case "Linux":
		return "linux", nil
	case "Darwin":
		return "darwin", nil
	case "Windows":
		return "windows", nil
	default:
		return "", fmt.Errorf("SSH target operating system %q is not supported", token)
	}
}

func normalizeSSHProbeArch(token string) (string, error) {
	switch token {
	case "x86_64", "amd64", "AMD64":
		return "amd64", nil
	case "aarch64", "arm64", "ARM64":
		return "arm64", nil
	default:
		return "", fmt.Errorf("SSH target architecture %q is not supported", token)
	}
}

func installSSH(ctx context.Context, target SSHTarget, platform sshPlatform) (string, error) {
	artifact, err := openSSHArtifact(target.Artifact, platform)
	if err != nil {
		return "", err
	}
	defer func() { _ = artifact.Close() }()
	h := sha256.New()
	if _, err := io.Copy(h, artifact); err != nil {
		return "", fmt.Errorf("read local SSH artifact: %w", err)
	}
	if _, err := artifact.Seek(0, io.SeekStart); err != nil {
		return "", fmt.Errorf("rewind local SSH artifact: %w", err)
	}
	digest := hex.EncodeToString(h.Sum(nil))
	command := posixInstall(digest)
	if platform.os == "windows" {
		command = powershell(windowsInstall(digest))
	}
	if _, err := runSSH(ctx, target.Destination, command, artifact); err != nil {
		return "", fmt.Errorf("install SHA-256 verified SSH artifact: %w", err)
	}
	return digest, nil
}

func openSSHArtifact(path string, platform sshPlatform) (*os.File, error) {
	if path != "" {
		return checkSSHArtifact(path, platform)
	}
	if exe, err := os.Executable(); err == nil {
		if file, err := checkSSHArtifact(exe, platform); err == nil {
			return file, nil
		}
	}
	name := "dalgona-" + platform.os + "-" + platform.arch
	if platform.os == "windows" {
		name += ".exe"
	}
	path = filepath.Join("dist", name)
	if file, err := checkSSHArtifact(path, platform); err == nil {
		return file, nil
	}
	return nil, fmt.Errorf("no local Dalgona CLI artifact for %s/%s; build %s with `just dist` or supply the matching local artifact path", platform.os, platform.arch, path)
}

func checkSSHArtifact(path string, platform sshPlatform) (*os.File, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("open local SSH artifact: %w", err)
	}
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() {
		_ = file.Close()
		return nil, fmt.Errorf("local SSH artifact %q is not a regular file", path)
	}
	if matchesSSHArtifact(file, platform) {
		return file, nil
	}
	_ = file.Close()
	return nil, fmt.Errorf("local SSH artifact %q is not a Dalgona CLI for %s/%s", path, platform.os, platform.arch)
}

func matchesSSHArtifact(file *os.File, platform sshPlatform) bool {
	info, err := buildinfo.Read(file)
	if err != nil || info.Path != "github.com/gosuda/dalgona/cmd/dalgona" {
		return false
	}
	var goos, goarch string
	for _, setting := range info.Settings {
		switch setting.Key {
		case "GOOS":
			goos = setting.Value
		case "GOARCH":
			goarch = setting.Value
		}
	}
	return goos == platform.os && goarch == platform.arch
}

func runSSH(ctx context.Context, destination, command string, in *os.File) (string, error) {
	cmd := exec.CommandContext(ctx, "ssh", sshArgs(destination, command)...)
	cmd.WaitDelay = 2 * time.Second
	var stdout, stderr sshDiagnostic
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if in != nil {
		cmd.Stdin = in
	}
	if err := cmd.Run(); err != nil {
		if ctx.Err() != nil {
			return "", context.Cause(ctx)
		}
		return "", fmt.Errorf("OpenSSH: %w: %s", err, stderr.String())
	}
	return stdout.String(), nil
}
