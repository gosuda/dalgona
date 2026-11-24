package platform

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"slices"
	"strings"
	"sync"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	toastAppID       = "Gosuda.Dalgona"
	toastActivatorID = "{4C824C81-04CD-4609-A755-D11526E6CDFE}"
	toastGroup       = "dalgona"
	toastTagUnits    = 64
	toastLaunchUnits = 4096
)

var (
	errToastText          = errors.New("the notification text holds a control character or invalid UTF-8 that a Windows notification cannot carry; rename the thread")
	errToastLaunch        = errors.New("the thread identifier is longer than 4096 UTF-16 units, so a Windows notification cannot reopen the thread; open it from the sidebar")
	toastEscaper          = strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", "\r", "&#xD;")
	toastAttributeEscaper = strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;", "\t", "&#x9;", "\n", "&#xA;", "\r", "&#xD;")
)

type winrtGUID struct {
	Data1 uint32
	Data2 uint16
	Data3 uint16
	Data4 [8]byte
}

var (
	iidToastActivatedHandler = winrtGUID{0xab54de2d, 0x97d9, 0x5528, [8]byte{0xb6, 0xad, 0x10, 0x5a, 0xfe, 0x15, 0x65, 0x30}}
	iidToastDismissedHandler = winrtGUID{0x61c2402f, 0x0ed0, 0x5a18, [8]byte{0xab, 0x69, 0x59, 0xf4, 0xaa, 0x99, 0xa3, 0x68}}
	iidToastFailedHandler    = winrtGUID{0x95e3e803, 0xc969, 0x5e3a, [8]byte{0x97, 0x53, 0xea, 0x2a, 0xd2, 0x2a, 0x9a, 0x33}}
	clsidToastActivator      = winrtGUID{0x4c824c81, 0x04cd, 0x4609, [8]byte{0xa7, 0x55, 0xd1, 0x15, 0x26, 0xe6, 0xcd, 0xfe}}
)

type toastActivations struct {
	mu   sync.Mutex
	tags []string
}

type toastSetting uint32

const (
	toastSettingEnabled toastSetting = iota
	toastSettingDisabledForApplication
	toastSettingDisabledForUser
	toastSettingDisabledByGroupPolicy
	toastSettingDisabledByManifest
)

func (s toastSetting) permission() NotificationPermission {
	switch s {
	case toastSettingEnabled:
		return PermissionGranted
	case toastSettingDisabledForApplication, toastSettingDisabledForUser, toastSettingDisabledByGroupPolicy:
		return PermissionDenied
	case toastSettingDisabledByManifest:
	}
	return PermissionUnavailable
}

func toastXML(title, body, launch string) (string, error) {
	if !toastText(title) || !toastText(body) || !toastText(launch) {
		return "", errToastText
	}
	if inputtext.Count(launch, inputtext.UTF16Units) > toastLaunchUnits {
		return "", errToastLaunch
	}
	var content strings.Builder
	content.WriteString(`<toast launch="`)
	_, _ = toastAttributeEscaper.WriteString(&content, launch)
	content.WriteString(`"><visual><binding template="ToastGeneric"><text>`)
	_, _ = toastEscaper.WriteString(&content, title)
	content.WriteString(`</text><text>`)
	_, _ = toastEscaper.WriteString(&content, body)
	content.WriteString(`</text></binding></visual></toast>`)
	return content.String(), nil
}

func toastText(text string) bool {
	return utf8.ValidString(text) && !strings.ContainsFunc(text, func(r rune) bool { return !xmlChar(r) })
}

func xmlChar(r rune) bool {
	return r == '\t' || r == '\n' || r == '\r' || 0x20 <= r && r <= 0xD7FF || 0xE000 <= r && r <= 0xFFFD || 0x10000 <= r && r <= 0x10FFFF
}

func toastTag(tag string) string {
	if inputtext.Count(tag, inputtext.UTF16Units) <= toastTagUnits {
		return tag
	}
	digest := sha256.Sum256([]byte(tag))
	return hex.EncodeToString(digest[:])
}

func toastActivation(app, launch []uint16) (string, bool) {
	if id, err := inputtext.DecodeUTF16(app); err != nil || id != toastAppID {
		return "", false
	}
	tag, err := inputtext.DecodeUTF16(launch)
	return tag, err == nil && tag != "" && toastText(tag)
}

func (a *toastActivations) fromActivator(tag string) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if len(a.tags) == notificationActivationQueue {
		a.tags = slices.Delete(a.tags, 0, 1)
	}
	a.tags = append(a.tags, tag)
}

func (a *toastActivations) fromToast(tag string) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	i := slices.Index(a.tags, tag)
	if i < 0 {
		return false
	}
	a.tags = slices.Delete(a.tags, i, i+1)
	return true
}
