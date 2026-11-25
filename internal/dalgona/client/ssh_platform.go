package client

import (
	"encoding/base64"
	"encoding/binary"
	"errors"
	"unicode/utf16"

	"github.com/gosuda/dalgona/internal/dal/config"
)

const posixProbe = `set -eu
os=$(uname -s)
arch=$(uname -m)
if [ "$os" = Darwin ] && [ "$(sysctl -in hw.optional.arm64)" = 1 ]; then arch=arm64; fi
installed=no
if [ "$(dalgona bridge --machine-version 2>/dev/null)" = dalgona-machine-v1 ]; then installed=yes; fi
printf 'dalgona-ssh-v1 %s %s %s\n' "$os" "$arch" "$installed"`

const windowsProbe = `$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT') { exit 1 }
$arch = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$installed = 'no'
$app = Get-Command dalgona.exe -CommandType Application -ErrorAction SilentlyContinue
if ($app -and ( (& $app.Source bridge --machine-version 2>$null) -ceq 'dalgona-machine-v1')) { $installed = 'yes' }
[Console]::Out.WriteLine('dalgona-ssh-v1 Windows ' + $arch + ' ' + $installed)`

func posixInstall(digest string) string {
	return `set -eu
umask 077
for tool in mkdir chmod mktemp cat ln rm rmdir; do command -v "$tool" >/dev/null; done
if command -v sha256sum >/dev/null 2>&1; then
  hash() { sha256sum; }
elif command -v shasum >/dev/null 2>&1; then
  hash() { shasum -a 256; }
else
  printf '%s\n' 'Install sha256sum or shasum on the SSH target' >&2
  exit 1
fi
root=${HOME:?}/.dalgona-bin
if [ -L "$root" ]; then exit 1; fi
mkdir -p "$root"
chmod 700 "$root"
stage=$(mktemp -d "$root/.stage.XXXXXXXX")
trap 'rm -f "$stage/dalgona"; rmdir "$stage"' EXIT
trap 'exit 1' HUP INT TERM
cat > "$stage/dalgona"
expected=` + digest + `
actual=$(hash < "$stage/dalgona")
if [ "${actual%% *}" != "$expected" ]; then
  printf '%s\n' 'Uploaded artifact failed SHA-256 verification' >&2
  exit 1
fi
chmod 700 "$stage/dalgona"
target=$root/dalgona-$expected
if [ ! -e "$target" ] && [ ! -L "$target" ] && ln "$stage/dalgona" "$target" 2>/dev/null; then exit 0; fi
if [ ! -f "$target" ] || [ -L "$target" ] || [ ! -x "$target" ]; then exit 1; fi
actual=$(hash < "$target")
if [ "${actual%% *}" != "$expected" ]; then
  printf '%s\n' 'Existing versioned artifact failed SHA-256 verification' >&2
  exit 1
fi`
}

func windowsInstall(digest string) string {
	return `$ErrorActionPreference = 'Stop'
$homePath = [Environment]::GetFolderPath('UserProfile')
if (!$homePath) { throw 'No Windows user profile' }
$root = [IO.Path]::Combine($homePath, '.dalgona-bin')
$owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = New-Object Security.AccessControl.DirectorySecurity
$acl.SetOwner($owner)
$acl.SetAccessRuleProtection($true, $false)
$rule = [Security.AccessControl.FileSystemAccessRule]::new($owner, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
$acl.AddAccessRule($rule)
if ([IO.Directory]::Exists($root)) {
  if (([IO.File]::GetAttributes($root) -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Invalid install directory' }
  [IO.Directory]::SetAccessControl($root, $acl)
} else {
  [void][IO.Directory]::CreateDirectory($root, $acl)
}
$stage = [IO.Path]::Combine($root, '.stage.' + [Guid]::NewGuid().ToString('N'))
$expected = '` + digest + `'
$target = [IO.Path]::Combine($root, 'dalgona-' + $expected + '.exe')
try {
  $file = [IO.File]::Open($stage, 'CreateNew', 'Write', 'None')
  try {
    [Console]::OpenStandardInput().CopyTo($file)
    $file.Flush($true)
  } finally { $file.Dispose() }
  if ((Get-FileHash -LiteralPath $stage -Algorithm SHA256).Hash -ine $expected) { throw 'Uploaded artifact failed SHA-256 verification' }
  try { [IO.File]::Move($stage, $target) }
  catch [IO.IOException] {
    if (![IO.File]::Exists($target)) { throw }
    if (([IO.File]::GetAttributes($target) -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Invalid versioned artifact' }
    if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ine $expected) { throw 'Existing versioned artifact failed SHA-256 verification' }
  }
} finally { [IO.File]::Delete($stage) }`
}

func sshBridgeCommand(spec config.RemoteSpec) (string, error) {
	if err := validateSSHBridgeSpec(spec); err != nil {
		return "", err
	}
	args := bridgeMachineArgs(spec.MachineID)
	return platformBridgeCommand(spec, args)
}

func validateSSHBridgeSpec(spec config.RemoteSpec) error {
	if spec.SSHInstall != "" && !config.ValidMachineDigest(spec.SSHInstall) {
		return errors.New("invalid SSH artifact digest")
	}
	if spec.MachineID != "" && !config.ValidMachineDigest(spec.MachineID) {
		return errors.New("invalid bound SSH machine identity")
	}
	return nil
}

func bridgeMachineArgs(machineID string) string {
	args := "bridge --machine"
	if machineID != "" {
		args += " " + machineID
	}
	return args
}

func platformBridgeCommand(spec config.RemoteSpec, args string) (string, error) {
	switch spec.SSHPlatform {
	case "linux", "darwin":
		return posixBridgeCommand(spec.SSHInstall, args), nil
	case "windows":
		return powershell(windowsBridge(spec.SSHInstall, args)), nil
	default:
		return "", errors.New("unsupported SSH shell platform")
	}
}

func posixBridgeCommand(digest, args string) string {
	if digest == "" {
		return "exec dalgona " + args
	}
	return `exec "${HOME:?}/.dalgona-bin/dalgona-` + digest + `" ` + args
}

func windowsBridge(digest, args string) string {
	path := `$path = (Get-Command dalgona.exe -CommandType Application -ErrorAction Stop).Source`
	if digest != "" {
		path = `$path = [IO.Path]::Combine([Environment]::GetFolderPath('UserProfile'), '.dalgona-bin', 'dalgona-` + digest + `.exe')`
	}
	return `$ErrorActionPreference = 'Stop'
` + path + `
$info = New-Object Diagnostics.ProcessStartInfo
$info.FileName = $path
$info.Arguments = '` + args + `'
$info.UseShellExecute = $false
$child = [Diagnostics.Process]::Start($info)
$child.WaitForExit()
exit $child.ExitCode`
}

func powershell(script string) string {
	utf := utf16.Encode([]rune(script))
	body := make([]byte, 0, len(utf)*2)
	for _, unit := range utf {
		body = binary.LittleEndian.AppendUint16(body, unit)
	}
	return "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand " + base64.StdEncoding.EncodeToString(body)
}
