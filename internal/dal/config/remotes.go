package config

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
)

const (
	RemoteBox    = "box"
	RemoteDaemon = "daemon"
	RemoteSSH    = "ssh"
	RemoteDevice = "device"

	tokenFileMaxBytes = 4096
)

var (
	errDaemonURL       = errors.New("must be an https:// or wss:// URL with a host and no user info, query, or fragment")
	errTokenNotRegular = errors.New("not a regular file")
)

func ParseDaemonURL(raw string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != "https" && u.Scheme != "wss" || u.Host == "" {
		return nil, errDaemonURL
	}
	if u.User != nil || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" {
		return nil, errDaemonURL
	}
	return u, nil
}

func ReadTokenFile(path string) (string, error) {
	file, err := openTokenFile(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return "", tokenFileError(path, "no such file")
	case errors.Is(err, errTokenNotRegular):
		return "", tokenFileError(path, errTokenNotRegular.Error())
	case err != nil:
		return "", fmt.Errorf("token_file %s: %w", path, err)
	}
	defer func() { _ = file.Close() }()
	if checkErr := checkTokenFile(file); checkErr != nil {
		return "", checkErr
	}
	data, err := io.ReadAll(io.LimitReader(file, tokenFileMaxBytes+1))
	switch {
	case err != nil:
		return "", fmt.Errorf("token_file %s: %w", path, err)
	case len(data) > tokenFileMaxBytes:
		return "", tokenFileError(path, "larger than 4096 bytes")
	}
	token := strings.TrimSpace(string(data))
	if token == "" {
		return "", tokenFileError(path, "holds no token")
	}
	return token, nil
}

func checkTokenFile(file *os.File) error {
	path := file.Name()
	info, err := file.Stat()
	switch {
	case err != nil:
		return fmt.Errorf("token_file %s: %w", path, err)
	case !info.Mode().IsRegular():
		return tokenFileError(path, errTokenNotRegular.Error())
	case runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0:
		return tokenFileError(path, "group or others have access; run chmod 600 "+path)
	case info.Size() > tokenFileMaxBytes:
		return tokenFileError(path, "larger than 4096 bytes")
	}
	return nil
}

func tokenFileError(path, reason string) error {
	return errors.New("token_file " + path + ": " + reason)
}

func (c *Config) mergeRemote(spec RemoteSpec, doc remoteDoc) RemoteSpec {
	set(doc.Kind, &spec.Kind)
	set(doc.Service, &spec.Service)
	set(doc.Image, &spec.Image)
	set(doc.Template, &spec.Template)
	set(doc.URL, &spec.URL)
	set(doc.BoxID, &spec.BoxID)
	set(doc.TokenFile, &spec.TokenFile)
	set(doc.MachineID, &spec.MachineID)
	set(doc.PublicKeyPin, &spec.PublicKeyPin)
	set(doc.SSHDestination, &spec.SSHDestination)
	set(doc.SSHPlatform, &spec.SSHPlatform)
	set(doc.SSHInstall, &spec.SSHInstall)
	if spec.TokenFile != "" && !filepath.IsAbs(spec.TokenFile) {
		spec.TokenFile = filepath.Join(c.Home, spec.TokenFile)
	}
	return spec
}

func (c *Config) validateRemotes() []error {
	var errs []error
	for _, name := range slices.Sorted(maps.Keys(c.Remotes)) {
		table := "remotes." + name
		if fault := checkRemote(table, c.Remotes[name]); fault != nil {
			errs = append(errs, c.remoteError(table, fault))
		}
	}
	return errs
}

func CheckRemote(name string, spec RemoteSpec) error {
	if fault := checkRemote("remotes."+name, spec); fault != nil {
		return fault
	}
	return nil
}

type remoteFault struct{ key, message string }

func (f *remoteFault) Error() string { return f.message }

func checkRemote(table string, spec RemoteSpec) *remoteFault {
	switch spec.Kind {
	case "", RemoteBox:
		if spec.TokenFile != "" {
			return &remoteFault{key: "token_file", message: table + ".token_file applies only to kind = \"daemon\" or \"device\""}
		}
		if spec.hasMachineBinding() {
			return &remoteFault{message: table + ": machine binding requires kind = \"ssh\" or \"device\""}
		}
		return nil
	case RemoteDaemon:
		return checkDaemonRemote(table, spec)
	case RemoteSSH:
		return checkSSHRemote(table, spec)
	case RemoteDevice:
		return checkDeviceRemote(table, spec)
	}
	return &remoteFault{key: "kind", message: table + ".kind must be one of box, daemon, ssh, device"}
}

func checkDaemonRemote(table string, spec RemoteSpec) *remoteFault {
	if spec.hasMachineBinding() {
		return &remoteFault{message: table + ": machine binding requires kind = \"ssh\" or \"device\""}
	}
	if spec.hasBoxSettings() {
		return &remoteFault{message: table + ": a daemon remote takes only kind, url, and token_file"}
	}
	if _, err := ParseDaemonURL(spec.URL); err != nil {
		return &remoteFault{key: "url", message: table + ".url " + err.Error()}
	}
	if spec.TokenFile == "" {
		return &remoteFault{message: table + ".token_file is required for a daemon remote"}
	}
	if _, err := ReadTokenFile(spec.TokenFile); err != nil {
		return &remoteFault{key: "token_file", message: table + "." + err.Error()}
	}
	return nil
}

func (c *Config) remoteError(table string, fault *remoteFault) error {
	if _, ok := c.lines[table+"."+fault.key]; ok && fault.key != "" {
		return c.valueError(table+"."+fault.key, fault.message)
	}
	return c.valueError(table, fault.message)
}
