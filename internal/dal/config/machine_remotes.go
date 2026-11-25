package config

import (
	"errors"
	"net"
	"net/url"
	"regexp"
	"strconv"
	"strings"
)

var sshDestination = regexp.MustCompile(`^(?:[A-Za-z0-9_][A-Za-z0-9_.-]*@)?(?:[A-Za-z0-9_][A-Za-z0-9_.:%-]*|\[[A-Fa-f0-9:.]+\])$`)

func ValidSSHDestination(value string) bool {
	return len(value) <= 255 && sshDestination.MatchString(value)
}

func ValidMachineDigest(value string) bool {
	if len(value) != 64 {
		return false
	}
	for _, c := range value {
		if !strings.ContainsRune("0123456789abcdef", c) {
			return false
		}
	}
	return true
}

func ParsePairingOrigin(raw string) (*url.URL, error) {
	u, err := ParseDaemonURL(raw)
	if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.RawPath != "" || u.Path != "" && u.Path != "/" {
		return nil, errors.New("pairing requires an HTTPS origin with no path, user, query, or fragment")
	}
	host, err := pairingAuthority(strings.ToLower(u.Hostname()), u.Port())
	if err != nil {
		return nil, err
	}
	return &url.URL{Scheme: "https", Host: host}, nil
}

func pairingAuthority(host, port string) (string, error) {
	if port != "" {
		value, err := strconv.ParseUint(port, 10, 16)
		if err != nil || value == 0 {
			return "", errors.New("pairing origin must have a valid port")
		}
		port = strconv.FormatUint(value, 10)
	}
	switch {
	case port != "" && port != "443":
		host = net.JoinHostPort(host, port)
	case strings.Contains(host, ":"):
		host = "[" + host + "]"
	}
	return host, nil
}

func (spec RemoteSpec) hasMachineBinding() bool {
	return spec.MachineID != "" || spec.PublicKeyPin != "" || spec.hasSSHBinding()
}

func (spec RemoteSpec) hasSSHBinding() bool {
	return spec.SSHDestination != "" || spec.SSHPlatform != "" || spec.SSHInstall != ""
}

func (spec RemoteSpec) hasBoxSettings() bool {
	return spec.Service != "" || spec.Image != "" || spec.Template != "" || spec.BoxID != ""
}

func checkSSHRemote(table string, spec RemoteSpec) *remoteFault {
	if spec.hasBoxSettings() || spec.URL != "" || spec.TokenFile != "" || spec.PublicKeyPin != "" {
		return &remoteFault{message: table + ": SSH remotes cannot use box or relay settings"}
	}
	return checkSSHBinding(table, spec)
}

func checkSSHBinding(table string, spec RemoteSpec) *remoteFault {
	switch {
	case !ValidSSHDestination(spec.SSHDestination):
		return &remoteFault{key: "ssh_destination", message: table + ".ssh_destination needs a host alias or user@host"}
	case !ValidMachineDigest(spec.MachineID):
		return &remoteFault{key: "machine_id", message: table + ".machine_id needs a bound machine identity; run dalgona onboard ssh"}
	case spec.SSHPlatform != "linux" && spec.SSHPlatform != "darwin" && spec.SSHPlatform != "windows":
		return &remoteFault{key: "ssh_platform", message: table + ".ssh_platform must be linux, darwin, or windows"}
	case spec.SSHInstall != "" && !ValidMachineDigest(spec.SSHInstall):
		return &remoteFault{key: "ssh_install", message: table + ".ssh_install must be a SHA-256 digest"}
	}
	return nil
}

func checkDeviceRemote(table string, spec RemoteSpec) *remoteFault {
	switch {
	case spec.hasBoxSettings() || spec.hasSSHBinding():
		return &remoteFault{message: table + ": device remotes cannot use box or SSH settings"}
	case !ValidMachineDigest(spec.MachineID):
		return &remoteFault{key: "machine_id", message: table + ".machine_id needs a bound machine identity; run dalgona pair join"}
	case !ValidMachineDigest(spec.PublicKeyPin):
		return &remoteFault{key: "public_key_pin", message: table + ".public_key_pin must be the relay's SHA-256 public-key pin"}
	case spec.TokenFile == "":
		return &remoteFault{key: "token_file", message: table + ".token_file is required for a device remote"}
	}
	if _, err := ParsePairingOrigin(spec.URL); err != nil {
		return &remoteFault{key: "url", message: table + ".url: " + err.Error()}
	}
	return nil
}
