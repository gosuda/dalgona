//go:build linux || freebsd || openbsd

package platform

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"errors"
	"fmt"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

func startX11Server(t *testing.T, prepare func(number int) []string) string {
	t.Helper()
	xvfb, err := exec.LookPath("Xvfb")
	if err != nil {
		t.Skip("Xvfb is not installed; install Xvfb to run the X11 wire tests")
	}
	first := 300 + rand.IntN(3000)
	for number := first; number < first+40; number++ {
		if _, err := os.Stat(fmt.Sprintf("/tmp/.X%d-lock", number)); err == nil {
			continue
		}
		if display, ok := tryX11Server(t, xvfb, number, prepare(number)); ok {
			return display
		}
	}
	t.Fatalf("Xvfb found no free display between :%d and :%d", first, first+39)
	return ""
}

func tryX11Server(t *testing.T, xvfb string, number int, options []string) (string, bool) {
	t.Helper()
	ready, readyWriter, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer ready.Close()
	server := exec.Command(xvfb, append([]string{":" + strconv.Itoa(number), "-displayfd", "3"}, options...)...)
	server.ExtraFiles = []*os.File{readyWriter}
	err = server.Start()
	closeErr := readyWriter.Close()
	if err != nil {
		t.Fatalf("start Xvfb: %v", err)
	}
	if closeErr != nil {
		_ = server.Process.Kill()
		_ = server.Wait()
		t.Fatal(closeErr)
	}
	if err = ready.SetReadDeadline(time.Now().Add(30 * time.Second)); err != nil {
		_ = server.Process.Kill()
		_ = server.Wait()
		t.Fatal(err)
	}
	line, err := bufio.NewReader(ready).ReadString('\n')
	if err != nil {
		_ = server.Process.Kill()
		_ = server.Wait()
		return "", false
	}
	t.Cleanup(func() {
		exited := make(chan struct{})
		go func() {
			defer close(exited)
			_ = server.Wait()
		}()
		_ = server.Process.Signal(syscall.SIGTERM)
		select {
		case <-exited:
		case <-time.After(10 * time.Second):
			_ = server.Process.Kill()
			<-exited
		}
	})
	return ":" + strings.TrimSpace(line), true
}

func withoutTCP(int) []string {
	return []string{"-nolisten", "tcp", "-noreset"}
}

func dialX11ForTest(t *testing.T, display string) *x11Conn {
	t.Helper()
	conn, err := dialX11(display)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.close() })
	return conn
}

type xauthLine struct {
	family  uint16
	address []byte
	number  string
	name    string
	cookie  string
}

func localXauthLine(t *testing.T, number int, name, cookie string) xauthLine {
	t.Helper()
	host, err := os.Hostname()
	if err != nil {
		t.Fatal(err)
	}
	return xauthLine{family: x11FamilyLocal, address: []byte(host), number: strconv.Itoa(number), name: name, cookie: cookie}
}

func writeXauthority(t *testing.T, path string, entries ...xauthLine) {
	t.Helper()
	xauth, err := exec.LookPath("xauth")
	if err != nil {
		t.Skip("xauth is not installed; install xauth to write the X authority fixtures")
	}
	var lines strings.Builder
	for _, e := range entries {
		fmt.Fprintf(&lines, "%04x %04x %x %04x %x %04x %x %04x %s\n", e.family, len(e.address), e.address, len(e.number), e.number, len(e.name), e.name, len(e.cookie)/2, e.cookie)
	}
	command := exec.Command(xauth, "-q", "-f", path, "nmerge", "-")
	command.Stdin = strings.NewReader(lines.String())
	if out, err := command.CombinedOutput(); err != nil {
		t.Fatalf("xauth nmerge: %v: %s", err, out)
	}
}

func TestX11DisplayNamesSelectTheTransport(t *testing.T) {
	cases := []struct {
		name    string
		network string
		address string
		number  string
		screen  uint64
	}{
		{":0", "unix", "/tmp/.X11-unix/X0", "0", 0},
		{":12.1", "unix", "/tmp/.X11-unix/X12", "12", 1},
		{"unix:3", "unix", "/tmp/.X11-unix/X3", "3", 0},
		{":007", "unix", "/tmp/.X11-unix/X7", "7", 0},
		{"localhost:10.0", "tcp", "localhost:6010", "10", 0},
		{"example.org:0", "tcp", "example.org:6000", "0", 0},
		{"[::1]:2", "tcp", "[::1]:6002", "2", 0},
		{"192.0.2.7:59535", "tcp", "192.0.2.7:65535", "59535", 0},
	}
	for _, c := range cases {
		got, err := parseX11Display(c.name)
		if err != nil {
			t.Fatalf("%q: %v", c.name, err)
		}
		want := x11Display{network: c.network, address: c.address, number: c.number, screen: c.screen}
		if got != want {
			t.Fatalf("%q parsed as %+v, want %+v", c.name, got, want)
		}
	}
	for _, name := range []string{"", "0", ":", ":x", ":1.", ":1.x", ":-1", ":+1", "host:59536", "/tmp/launch:0", ":1.256"} {
		if _, err := parseX11Display(name); err == nil {
			t.Fatalf("%q parsed as a display name", name)
		}
	}
}

func xauthorityFixture(t *testing.T, entries ...xauthLine) []byte {
	t.Helper()
	path := filepath.Join(t.TempDir(), "authority")
	writeXauthority(t, path, entries...)
	authority, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return authority
}

func TestX11AuthorityFindsTheEntryForThisDisplay(t *testing.T) {
	remoteHost := []byte{10, 1, 2, 3}
	authority := xauthorityFixture(t,
		localXauthLine(t, 41, x11CookieName, "41414141414141414141414141414141"),
		xauthLine{family: x11FamilyInternet, address: remoteHost, number: "40", name: x11CookieName, cookie: "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"},
		localXauthLine(t, 42, "XDM-AUTHORIZATION-1", "deadbeefdeadbeefdeadbeefdeadbeef"),
		localXauthLine(t, 40, x11CookieName, "40404040404040404040404040404040"),
	)
	host, err := os.Hostname()
	if err != nil {
		t.Fatal(err)
	}
	local := x11AuthAddress{family: x11FamilyLocal, address: []byte(host)}
	remote := x11AuthAddress{family: x11FamilyInternet, address: remoteHost}
	other := x11AuthAddress{family: x11FamilyInternet, address: []byte{10, 1, 2, 4}}
	cases := []struct {
		peer   x11AuthAddress
		number string
		want   string
	}{
		{local, "40", "40404040404040404040404040404040"},
		{local, "41", "41414141414141414141414141414141"},
		{remote, "40", "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"},
		{remote, "41", ""},
		{other, "40", ""},
		{local, "42", ""},
		{local, "43", ""},
	}
	for _, c := range cases {
		got := hex.EncodeToString(findX11Cookie(authority, c.peer, c.number))
		if got != c.want {
			t.Fatalf("cookie for family %d display %s = %q, want %q", c.peer.family, c.number, got, c.want)
		}
	}
	single := xauthorityFixture(t, localXauthLine(t, 40, x11CookieName, "40404040404040404040404040404040"))
	if got := findX11Cookie(single[:len(single)-1], local, "40"); got != nil {
		t.Fatalf("a truncated entry granted %x", got)
	}
	wild := xauthorityFixture(t, xauthLine{family: x11FamilyWild, name: x11CookieName, cookie: "77777777777777777777777777777777"})
	if got := hex.EncodeToString(findX11Cookie(wild, other, "9")); got != "77777777777777777777777777777777" {
		t.Fatalf("a wildcard entry granted %q", got)
	}
}

func TestX11ConnectsWithTheSessionCookie(t *testing.T) {
	path := filepath.Join(t.TempDir(), "authority")
	wrong := filepath.Join(t.TempDir(), "wrong")
	display := startX11Server(t, func(number int) []string {
		_ = os.Remove(path)
		_ = os.Remove(wrong)
		writeXauthority(t, path,
			localXauthLine(t, number+1, x11CookieName, "11111111111111111111111111111111"),
			localXauthLine(t, number, x11CookieName, "22222222222222222222222222222222"),
		)
		writeXauthority(t, wrong, localXauthLine(t, number, x11CookieName, "33333333333333333333333333333333"))
		return []string{"-listen", "tcp", "-noreset", "-auth", path}
	})
	t.Setenv("XAUTHORITY", path)
	for _, name := range []string{display, "unix" + display, "localhost" + display} {
		conn, err := dialX11(name)
		if err != nil {
			t.Fatalf("connect to %s with the session cookie: %v", name, err)
		}
		if _, err := conn.internAtoms([]string{"CLIPBOARD"}); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		_ = conn.close()
	}
	for _, authority := range []string{wrong, filepath.Join(t.TempDir(), "missing")} {
		t.Setenv("XAUTHORITY", authority)
		conn, err := dialX11(display)
		if err == nil {
			_ = conn.close()
		}
		if err == nil || !strings.Contains(err.Error(), "refused the connection") {
			t.Fatalf("XAUTHORITY=%s connected or failed differently: %v", authority, err)
		}
	}
}

func TestX11ConnRefusesRequestsBeyondTheServerMaximum(t *testing.T) {
	conn := dialX11ForTest(t, startX11Server(t, withoutTCP))
	window, err := conn.createInputWindow(0)
	if err != nil {
		t.Fatal(err)
	}
	fits := make([]byte, 4*conn.maxRequestUnits-24)
	oversized := make([]byte, len(fits)+1)
	write := x11PropertyWrite{window: window, property: x11AtomString, kind: x11AtomString, format: 8, data: oversized}
	if err = conn.changeProperty(write); err == nil {
		t.Fatalf("a %d-byte ChangeProperty request was sent past the %d-unit maximum", len(oversized)+24, conn.maxRequestUnits)
	}
	write.data = fits
	if err = conn.changeProperty(write); err != nil {
		t.Fatal(err)
	}
	stored, err := conn.readProperty(window, x11AtomString, nil)
	if err != nil {
		t.Fatalf("the connection failed after the refused request: %v", err)
	}
	if !bytes.Equal(stored.value, fits) || stored.format != 8 || stored.kind != x11AtomString {
		t.Fatalf("the property holds %d bytes of type %d format %d, want %d bytes of STRING format 8", len(stored.value), stored.kind, stored.format, len(fits))
	}
}

func TestX11ErrorsNameTheRejectedRequest(t *testing.T) {
	conn := dialX11ForTest(t, startX11Server(t, withoutTCP))
	window, err := conn.createInputWindow(0)
	if err != nil {
		t.Fatal(err)
	}
	if err = conn.destroyWindow(window); err != nil {
		t.Fatal(err)
	}
	_, err = conn.readProperty(window, x11AtomString, nil)
	var rejected *x11Error
	if !errors.As(err, &rejected) || rejected.code != 3 || rejected.major != x11OpGetProperty || rejected.value != window {
		t.Fatalf("GetProperty on a destroyed window returned %v, want a Window error for request 20 naming %#x", err, window)
	}
	if _, err := conn.internAtoms([]string{"PRIMARY"}); err != nil {
		t.Fatalf("the connection failed after the protocol error: %v", err)
	}
}
