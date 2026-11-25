package platform

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"unicode/utf8"
)

const appKitNotificationTagLimit = 4096

const (
	appKitAuthorizationNotDetermined = iota
	appKitAuthorizationDenied
	appKitAuthorizationAuthorized
	appKitAuthorizationProvisional
	appKitAuthorizationUnknown
)

var appKitPermissions = [...]NotificationPermission{
	appKitAuthorizationNotDetermined: PermissionUndecided,
	appKitAuthorizationDenied:        PermissionDenied,
	appKitAuthorizationAuthorized:    PermissionGranted,
	appKitAuthorizationProvisional:   PermissionGranted,
	appKitAuthorizationUnknown:       PermissionUnavailable,
}

var (
	errNotificationActivation = errors.New("the macOS notification bridge sent a malformed click record; restart dalgona to receive notification clicks again")
	errNotificationTag        = errors.New("the notification has no tag or a tag over 4096 bytes; send it with its thread identifier")
	errNotificationText       = errors.New("the notification text is not valid UTF-8; send valid text")
	errNotificationPermission = errors.New("macOS reported an unknown notification permission; open System Settings, choose Notifications, then check dalgona")
)

func appKitNotificationPermission(status int, failure string) (NotificationPermission, error) {
	permission := PermissionUnavailable
	if status >= 0 && status < len(appKitPermissions) {
		permission = appKitPermissions[status]
	}
	switch {
	case failure != "":
		return permission, fmt.Errorf("macOS did not complete the notification permission request: %s; allow notifications for dalgona in System Settings", failure)
	case permission == PermissionUnavailable:
		return permission, errNotificationPermission
	}
	return permission, nil
}

func checkAppKitNotification(note Notification) error {
	if note.Tag == "" || len(note.Tag) > appKitNotificationTagLimit {
		return errNotificationTag
	}
	if !utf8.ValidString(note.Tag) || !utf8.ValidString(note.Title) || !utf8.ValidString(note.Body) {
		return errNotificationText
	}
	return nil
}

func readNotificationTag(r io.Reader, header *[4]byte) (string, error) {
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return "", err
	}
	length := binary.LittleEndian.Uint32(header[:])
	if length == 0 || length > appKitNotificationTagLimit {
		return "", errNotificationActivation
	}
	tag := make([]byte, length)
	if read, err := io.ReadFull(r, tag); err != nil {
		return "", fmt.Errorf("%w: the record ended after %d of %d bytes", errNotificationActivation, read, length)
	}
	if !utf8.Valid(tag) {
		return "", errNotificationActivation
	}
	return string(tag), nil
}

func serveNotificationActivations(r io.Reader, activate func(string)) error {
	var header [4]byte
	for {
		tag, err := readNotificationTag(r, &header)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		activate(tag)
	}
}
