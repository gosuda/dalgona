package config_test

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

const daemonRemoteTable = "[remotes.work]\nkind = \"daemon\"\n"

func daemonRemoteBody(url, tokenFile string) string {
	return daemonRemoteTable + "url = \"" + url + "\"\ntoken_file = \"" + tokenFile + "\"\n"
}

func TestDaemonRemoteLoadsKindURLAndTokenFile(t *testing.T) {
	home := t.TempDir()
	writeConfigFile(t, home, "work.token", "relay-token\n")
	cfg, err := loadTLSConfig(t, home, daemonRemoteBody("wss://work.example:7437", "work.token"))
	if err != nil {
		t.Fatal(err)
	}
	want := config.RemoteSpec{Kind: config.RemoteDaemon, URL: "wss://work.example:7437", TokenFile: filepath.Join(home, "work.token")}
	if got := cfg.Remotes["work"]; got != want {
		t.Fatalf("remotes.work = %+v, want %+v", got, want)
	}
}

func TestDaemonRemoteRejectsURLsWithoutTLSOrWithSecrets(t *testing.T) {
	home := t.TempDir()
	writeConfigFile(t, home, "work.token", "relay-token\n")
	want := "config.toml:3: remotes.work.url must be an https:// or wss:// URL with a host and no user info, query, or fragment"
	for _, url := range []string{
		"http://work.example:7437",
		"ws://work.example:7437",
		"work.example:7437",
		"https://",
		"https://user:secret@work.example",
		"https://work.example/?token=relay-token",
		"wss://work.example/#token=relay-token",
	} {
		t.Run(url, func(t *testing.T) {
			_, err := loadTLSConfig(t, home, daemonRemoteBody(url, "work.token"))
			requireConfigError(t, err, want)
		})
	}
	t.Run("absent", func(t *testing.T) {
		_, err := loadTLSConfig(t, home, daemonRemoteTable+"token_file = \"work.token\"\n")
		requireConfigError(t, err, "config.toml:1: remotes.work.url must be an https:// or wss:// URL with a host and no user info, query, or fragment")
	})
}

func TestDaemonRemoteRejectsTokenFilesOthersCanRead(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the product home ACL protects secret files on Windows")
	}
	home := t.TempDir()
	path := filepath.Join(home, "work.token")
	writeConfigFile(t, home, "work.token", "relay-token\n")
	for _, mode := range []os.FileMode{0o640, 0o604, 0o620, 0o602} {
		t.Run(mode.String(), func(t *testing.T) {
			if err := os.Chmod(path, mode); err != nil {
				t.Fatal(err)
			}
			_, err := loadTLSConfig(t, home, daemonRemoteBody("https://work.example", "work.token"))
			requireConfigError(t, err, "config.toml:4: remotes.work.token_file "+path+": group or others have access; run chmod 600 "+path)
		})
	}
}

func TestDaemonRemoteTokenFileErrors(t *testing.T) {
	home := t.TempDir()
	if err := os.Mkdir(filepath.Join(home, "dir.token"), 0o700); err != nil {
		t.Fatal(err)
	}
	writeConfigFile(t, home, "blank.token", " \n\t\n")
	writeConfigFile(t, home, "large.token", strings.Repeat("k", 4097))
	for _, test := range []struct{ file, want string }{
		{"missing.token", "no such file"},
		{"dir.token", "not a regular file"},
		{"blank.token", "holds no token"},
		{"large.token", "larger than 4096 bytes"},
	} {
		t.Run(test.file, func(t *testing.T) {
			_, err := loadTLSConfig(t, home, daemonRemoteBody("https://work.example", test.file))
			requireConfigError(t, err, "config.toml:4: remotes.work.token_file "+filepath.Join(home, test.file)+": "+test.want)
		})
	}
	t.Run("absent", func(t *testing.T) {
		_, err := loadTLSConfig(t, home, daemonRemoteTable+"url = \"https://work.example\"\n")
		requireConfigError(t, err, "config.toml:1: remotes.work.token_file is required for a daemon remote")
	})
}

func TestRemoteKindsKeepTheirOwnKeys(t *testing.T) {
	home := t.TempDir()
	writeConfigFile(t, home, "work.token", "relay-token\n")
	for _, test := range []struct{ name, body, want string }{
		{
			"unknown kind",
			"[remotes.work]\nkind = \"not-a-transport\"\n",
			"config.toml:2: remotes.work.kind must be one of box, daemon, ssh, device",
		},
		{
			"box keys on a daemon remote",
			daemonRemoteBody("https://work.example", "work.token") + "service = \"docker\"\n",
			"config.toml:1: remotes.work: a daemon remote takes only kind, url, and token_file",
		},
		{
			"token file on a box remote",
			"[remotes.work]\nservice = \"docker\"\ntoken_file = \"work.token\"\n",
			"config.toml:3: remotes.work.token_file applies only to kind = \"daemon\" or \"device\"",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, err := loadTLSConfig(t, home, test.body)
			requireConfigError(t, err, test.want)
		})
	}
	cfg, err := loadTLSConfig(t, home, "[remotes.work]\nkind = \"box\"\nservice = \"docker\"\n")
	if err != nil || cfg.Remotes["work"] != (config.RemoteSpec{Kind: config.RemoteBox, Service: "docker"}) {
		t.Fatalf("explicit box remote = %+v, %v", cfg.Remotes["work"], err)
	}
}
