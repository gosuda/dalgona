//go:build linux

package platform

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"
	"unicode"

	"github.com/godbus/dbus/v5"
)

const (
	portalService     = "org.freedesktop.portal.Desktop"
	portalObject      = "/org/freedesktop/portal/desktop"
	portalFileChooser = "org.freedesktop.portal.FileChooser"
	portalRequest     = "org.freedesktop.portal.Request"
)

const (
	portalResponseSuccess   = 0
	portalResponseCancelled = 1
)

var errPortalEnded = errors.New("the file chooser closed without a selection; open it again")

type portalPattern struct {
	Kind    uint32
	Pattern string
}

type portalFilter struct {
	Name     string
	Patterns []portalPattern
}

type portalCall struct {
	method  string
	args    []any
	options map[string]dbus.Variant
}

type portalAnswer struct {
	code    uint32
	results map[string]dbus.Variant
}

func PortalDialog(ctx context.Context, conn *dbus.Conn, parent string, req DialogRequest) (DialogResult, error) {
	if err := req.Check(); err != nil {
		return DialogResult{}, err
	}
	answer, err := askPortal(ctx, conn, fileChooserCall(parent, req))
	if err != nil {
		return DialogResult{}, err
	}
	return dialogResult(answer)
}

func askPortal(ctx context.Context, conn *dbus.Conn, call portalCall) (portalAnswer, error) {
	token, err := portalToken()
	if err != nil {
		return portalAnswer{}, err
	}
	call.options["handle_token"] = dbus.MakeVariant(token)
	responses := make(chan *dbus.Signal, 4)
	conn.Signal(responses)
	defer conn.RemoveSignal(responses)
	expected := portalRequestPath(conn, token)
	if err = watchPortalRequest(conn, expected); err != nil {
		return portalAnswer{}, err
	}
	defer unwatchPortalRequest(conn, expected)
	handle, err := callPortal(ctx, conn, call)
	if err != nil {
		return portalAnswer{}, err
	}
	if handle != expected {
		if err := watchPortalRequest(conn, handle); err != nil {
			return portalAnswer{}, err
		}
		defer unwatchPortalRequest(conn, handle)
	}
	return awaitPortalResponse(ctx, conn, handle, responses)
}

func portalToken() (string, error) {
	var random [16]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", fmt.Errorf("create a portal request token: %w", err)
	}
	return "dalgona" + hex.EncodeToString(random[:]), nil
}

func portalRequestPath(conn *dbus.Conn, token string) dbus.ObjectPath {
	sender := strings.ReplaceAll(strings.TrimPrefix(conn.Names()[0], ":"), ".", "_")
	return dbus.ObjectPath(portalObject + "/request/" + sender + "/" + token)
}

func watchPortalRequest(conn *dbus.Conn, path dbus.ObjectPath) error {
	err := conn.AddMatchSignal(dbus.WithMatchObjectPath(path), dbus.WithMatchInterface(portalRequest), dbus.WithMatchMember("Response"))
	if err != nil {
		return fmt.Errorf("subscribe to the portal answer: %w", err)
	}
	return nil
}

func unwatchPortalRequest(conn *dbus.Conn, path dbus.ObjectPath) {
	_ = conn.RemoveMatchSignal(dbus.WithMatchObjectPath(path), dbus.WithMatchInterface(portalRequest), dbus.WithMatchMember("Response"))
}

func callPortal(ctx context.Context, conn *dbus.Conn, call portalCall) (dbus.ObjectPath, error) {
	var handle dbus.ObjectPath
	args := make([]any, 0, len(call.args)+1)
	args = append(args, call.args...)
	args = append(args, call.options)
	err := conn.Object(portalService, portalObject).CallWithContext(ctx, call.method, 0, args...).Store(&handle)
	var dbusErr dbus.Error
	if errors.As(err, &dbusErr) && portalMissing(dbusErr.Name) {
		return "", ErrPortalUnavailable
	}
	if err != nil {
		return "", fmt.Errorf("call the desktop portal %s: %w", call.method, err)
	}
	return handle, nil
}

func portalMissing(name string) bool {
	return name == "org.freedesktop.DBus.Error.ServiceUnknown" || name == "org.freedesktop.DBus.Error.UnknownMethod" ||
		name == "org.freedesktop.DBus.Error.UnknownInterface" || name == "org.freedesktop.DBus.Error.UnknownObject"
}

func fileChooserCall(parent string, req DialogRequest) portalCall {
	call := portalCall{
		method:  portalFileChooser + ".OpenFile",
		args:    []any{parent, req.Title},
		options: map[string]dbus.Variant{"modal": dbus.MakeVariant(true)},
	}
	if len(req.Filters) > 0 {
		call.options["filters"] = dbus.MakeVariant(portalFilters(req.Filters))
	}
	switch req.Kind {
	case DialogOpenFiles:
		call.options["multiple"] = dbus.MakeVariant(true)
	case DialogChooseFolder:
		call.options["directory"] = dbus.MakeVariant(true)
	case DialogSaveFile:
		call.options["current_name"] = dbus.MakeVariant(req.SuggestedName)
		call.method = portalFileChooser + ".SaveFile"
	}
	return call
}

func portalFilters(filters []FileFilter) []portalFilter {
	out := make([]portalFilter, 0, len(filters))
	for _, f := range filters {
		patterns := make([]portalPattern, 0, len(f.Extensions))
		for _, ext := range f.Extensions {
			patterns = append(patterns, portalPattern{Pattern: "*." + anyCaseGlob(ext)})
		}
		out = append(out, portalFilter{Name: f.Name, Patterns: patterns})
	}
	return out
}

func anyCaseGlob(ext string) string {
	var glob strings.Builder
	for _, r := range ext {
		lower, upper := unicode.ToLower(r), unicode.ToUpper(r)
		if lower == upper {
			glob.WriteRune(r)
			continue
		}
		glob.WriteString("[" + string(lower) + string(upper) + "]")
	}
	return glob.String()
}

func awaitPortalResponse(ctx context.Context, conn *dbus.Conn, handle dbus.ObjectPath, responses <-chan *dbus.Signal) (portalAnswer, error) {
	for {
		select {
		case <-ctx.Done():
			conn.Object(portalService, handle).Call(portalRequest+".Close", 0)
			return portalAnswer{}, ctx.Err()
		case s, ok := <-responses:
			if !ok {
				return portalAnswer{}, fmt.Errorf("the file chooser closed while the dialog waited: %w", errPortalEnded)
			}
			if s.Path != handle || s.Name != portalRequest+".Response" {
				continue
			}
			var answer portalAnswer
			if err := dbus.Store(s.Body, &answer.code, &answer.results); err != nil {
				return portalAnswer{}, fmt.Errorf("read the portal answer: %w", err)
			}
			return answer, nil
		}
	}
}

func dialogResult(answer portalAnswer) (DialogResult, error) {
	switch answer.code {
	case portalResponseSuccess:
	case portalResponseCancelled:
		return DialogResult{Cancelled: true}, nil
	default:
		return DialogResult{}, errPortalEnded
	}
	raw, ok := answer.results["uris"]
	if !ok {
		return DialogResult{}, errPortalEnded
	}
	var uris []string
	if err := raw.Store(&uris); err != nil || len(uris) == 0 {
		return DialogResult{}, errPortalEnded
	}
	files := make([]string, 0, len(uris))
	for _, uri := range uris {
		path, err := LocalFile(uri)
		if err != nil {
			return DialogResult{}, err
		}
		files = append(files, path)
	}
	return DialogResult{Files: files}, nil
}
