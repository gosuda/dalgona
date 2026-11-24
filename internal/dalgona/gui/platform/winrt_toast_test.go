package platform

import (
	"crypto/sha1" //nolint:gosec // the WinRT parameterized interface ID algorithm is defined over SHA-1
	"encoding/binary"
	"encoding/hex"
	"encoding/xml"
	"errors"
	"strconv"
	"strings"
	"testing"
	"unicode/utf16"
)

type toastDocument struct {
	XMLName xml.Name `xml:"toast"`
	Launch  string   `xml:"launch,attr"`
	Visual  struct {
		Binding struct {
			Template string   `xml:"template,attr"`
			Text     []string `xml:"text"`
		} `xml:"binding"`
	} `xml:"visual"`
}

func parseToast(t *testing.T, content string) toastDocument {
	t.Helper()
	var document toastDocument
	if err := xml.Unmarshal([]byte(content), &document); err != nil {
		t.Fatalf("the toast content is not well-formed XML: %v\n%s", err, content)
	}
	return document
}

func TestToastXMLCarriesTitleBodyAndLaunchTagExactly(t *testing.T) {
	cases := []struct{ title, body, launch string }{
		{"Waiting for your reply", "release notes", "thread-01J9Z3"},
		{"Done", `a & b < c > d "quoted" 'single' ]]> </text><text>x`, `local/a"b&c<d>'e`},
		{"Failed", "first\r\nsecond\rthird\nfourth\tend", "tab\tline\ncarriage\rend"},
		{"Stopped", "한글 e\u0301 \U0001F469\u200D\U0001F4BB \uFFFD \uE000 \uD7FF \U0010FFFF", "remote/한글\U0001F600"},
		{"", "", ""},
	}
	for _, tc := range cases {
		content, err := toastXML(tc.title, tc.body, tc.launch)
		if err != nil {
			t.Fatalf("toastXML(%q, %q, %q): %v", tc.title, tc.body, tc.launch, err)
		}
		document := parseToast(t, content)
		binding := document.Visual.Binding
		if binding.Template != "ToastGeneric" {
			t.Errorf("template %q, want ToastGeneric", binding.Template)
		}
		if len(binding.Text) != 2 || binding.Text[0] != tc.title || binding.Text[1] != tc.body {
			t.Errorf("toastXML(%q, %q) parsed back to %q", tc.title, tc.body, binding.Text)
		}
		if document.Launch != tc.launch {
			t.Errorf("launch %q parsed back to %q", tc.launch, document.Launch)
		}
		if opening := content[:strings.IndexByte(content, '>')]; strings.ContainsAny(opening, "\t\n\r") {
			t.Errorf("the launch attribute %q holds literal whitespace that XML attribute normalization turns into spaces", opening)
		}
	}
}

func TestToastXMLRefusesTextXMLCannotCarry(t *testing.T) {
	for _, text := range []string{"\x00", "a\x01b", "\x08", "\x0B", "\x0C", "\x1F", "\uFFFE", "\uFFFF", "\xed\xa0\x80", "\xff", "a\xc3"} {
		if content, err := toastXML("Done", text, "thread"); !errors.Is(err, errToastText) || content != "" {
			t.Errorf("toastXML body %q = %q, %v; want errToastText", text, content, err)
		}
		if content, err := toastXML(text, "thread", "thread"); !errors.Is(err, errToastText) || content != "" {
			t.Errorf("toastXML title %q = %q, %v; want errToastText", text, content, err)
		}
		if content, err := toastXML("Done", "thread", text); !errors.Is(err, errToastText) || content != "" {
			t.Errorf("toastXML launch %q = %q, %v; want errToastText", text, content, err)
		}
	}
}

func TestToastXMLBoundsTheLaunchTagAt4096UTF16Units(t *testing.T) {
	fits := strings.Repeat("\U0001F600", 2048)
	if _, err := toastXML("Done", "thread", fits); err != nil {
		t.Fatalf("a launch tag of 4096 UTF-16 units: %v", err)
	}
	if content, err := toastXML("Done", "thread", fits+"a"); !errors.Is(err, errToastLaunch) || content != "" {
		t.Fatalf("a launch tag of 4097 UTF-16 units = %q, %v; want errToastLaunch", content, err)
	}
}

func TestToastActivationAcceptsOnlyDalgonaTags(t *testing.T) {
	units := func(s string) []uint16 { return utf16.Encode([]rune(s)) }
	app := units("Gosuda.Dalgona")
	cases := []struct {
		app, launch []uint16
		tag         string
		ok          bool
	}{
		{app, units("thread-01J9Z3"), "thread-01J9Z3", true},
		{app, units("remote/한글\U0001F600"), "remote/한글\U0001F600", true},
		{units("Other.App"), units("thread"), "", false},
		{units("Gosuda.Dalgona2"), units("thread"), "", false},
		{app, nil, "", false},
		{app, []uint16{'a', 0xD800}, "", false},
		{app, []uint16{'a', 0xDC00, 'b'}, "", false},
		{app, units("a\x01b"), "", false},
	}
	for _, tc := range cases {
		tag, ok := toastActivation(tc.app, tc.launch)
		if ok != tc.ok || ok && tag != tc.tag {
			t.Errorf("toastActivation(%q, %#04x) = %q, %v; want %q, %v", string(utf16.Decode(tc.app)), tc.launch, tag, ok, tc.tag, tc.ok)
		}
	}
}

func TestToastActivationsReportEachClickOnce(t *testing.T) {
	var clicks toastActivations
	clicks.fromActivator("t1")
	if !clicks.fromToast("t1") {
		t.Fatal("the in-process Activated event repeated a click the activator already reported")
	}
	if clicks.fromToast("t1") {
		t.Fatal("a later in-process click on t1 was dropped although the activator did not report it")
	}
	for i := range 3 * notificationActivationQueue {
		clicks.fromActivator(strconv.Itoa(i))
	}
	if len(clicks.tags) != notificationActivationQueue {
		t.Fatalf("%d remembered activator clicks, want at most %d", len(clicks.tags), notificationActivationQueue)
	}
	if clicks.fromToast("0") || !clicks.fromToast(strconv.Itoa(3*notificationActivationQueue-1)) {
		t.Fatal("the activator memory did not keep the newest clicks")
	}
}

func TestToastActivatorCLSIDMatchesItsRegisteredString(t *testing.T) {
	fields := strings.Split(strings.TrimSuffix(strings.TrimPrefix(toastActivatorID, "{"), "}"), "-")
	raw, err := hex.DecodeString(strings.Join(fields, ""))
	if err != nil || len(fields) != 5 || len(fields[0]) != 8 || len(fields[4]) != 12 || len(raw) != 16 || strings.ToUpper(toastActivatorID) != toastActivatorID {
		t.Fatalf("%s is not a braced uppercase registry GUID: %v", toastActivatorID, err)
	}
	want := winrtGUID{Data1: binary.BigEndian.Uint32(raw[0:4]), Data2: binary.BigEndian.Uint16(raw[4:6]), Data3: binary.BigEndian.Uint16(raw[6:8])}
	copy(want.Data4[:], raw[8:])
	if clsidToastActivator != want {
		t.Fatalf("the registered activator %s is %+v in memory, want %+v", toastActivatorID, clsidToastActivator, want)
	}
}

func TestToastTagKeepsSixtyFourUnitsAndDigestsLongerTags(t *testing.T) {
	kept := []string{
		"thread-01J9Z3",
		strings.Repeat("a", 64),
		strings.Repeat("한", 64),
		strings.Repeat("\U0001F600", 32),
	}
	for _, tag := range kept {
		if got := toastTag(tag); got != tag {
			t.Errorf("toastTag(%q) = %q, want the tag unchanged", tag, got)
		}
	}
	long := []string{strings.Repeat("a", 65), strings.Repeat("\U0001F600", 32) + "a", strings.Repeat("b", 65)}
	seen := map[string]string{}
	for _, tag := range long {
		got := toastTag(tag)
		if len(got) != 64 || strings.Trim(got, "0123456789abcdef") != "" || got == tag {
			t.Errorf("toastTag(%q) = %q, want 64 lowercase hex digits", tag, got)
		}
		if toastTag(tag) != got {
			t.Errorf("toastTag(%q) changed between calls", tag)
		}
		if other, ok := seen[got]; ok {
			t.Errorf("toastTag gave %q and %q the same key", other, tag)
		}
		seen[got] = tag
	}
}

func TestToastSettingFollowsNotificationSetting(t *testing.T) {
	want := []NotificationPermission{PermissionGranted, PermissionDenied, PermissionDenied, PermissionDenied, PermissionUnavailable, PermissionUnavailable}
	for value, permission := range want {
		if got := toastSetting(value).permission(); got != permission {
			t.Errorf("NotificationSetting %d gave permission %d, want %d", value, got, permission)
		}
	}
}

func winrtParameterizedIID(signature string) winrtGUID {
	namespace := [16]byte{0x11, 0xf4, 0x7a, 0xd5, 0x7b, 0x73, 0x42, 0xc0, 0xab, 0xae, 0x87, 0x8b, 0x1e, 0x16, 0xad, 0xee}
	digest := sha1.Sum(append(namespace[:], signature...)) //nolint:gosec // the WinRT parameterized interface ID algorithm is defined over SHA-1
	digest[6] = digest[6]&0x0f | 0x50
	digest[8] = digest[8]&0x3f | 0x80
	var iid winrtGUID
	iid.Data1 = binary.BigEndian.Uint32(digest[0:4])
	iid.Data2 = binary.BigEndian.Uint16(digest[4:6])
	iid.Data3 = binary.BigEndian.Uint16(digest[6:8])
	copy(iid.Data4[:], digest[8:16])
	return iid
}

func TestToastHandlerIIDsFollowTheWinRTParameterizedAlgorithm(t *testing.T) {
	const (
		typedEventHandler = "{9de1c534-6ae1-11e0-84e1-18a905bcc53f}"
		toastNotification = "rc(Windows.UI.Notifications.ToastNotification;{997e2675-059e-4e60-8b06-1760917c8b80})"
	)
	cases := []struct {
		name string
		args string
		iid  winrtGUID
	}{
		{"Activated", "cinterface(IInspectable)", iidToastActivatedHandler},
		{"Dismissed", "rc(Windows.UI.Notifications.ToastDismissedEventArgs;{3f89d935-d9cb-4538-a0f0-ffe7659938f8})", iidToastDismissedHandler},
		{"Failed", "rc(Windows.UI.Notifications.ToastFailedEventArgs;{35176862-cfd4-44f8-ad64-f500fd896c3b})", iidToastFailedHandler},
	}
	for _, tc := range cases {
		signature := "pinterface(" + typedEventHandler + ";" + toastNotification + ";" + tc.args + ")"
		if got := winrtParameterizedIID(signature); got != tc.iid {
			t.Errorf("%s handler IID %+v, but the signature %s hashes to %+v", tc.name, tc.iid, signature, got)
		}
	}
}
