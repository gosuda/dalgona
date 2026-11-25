package platform

import (
	"bytes"
	"strings"
	"testing"
)

const evidenceCommit = "0123456789abcdef0123456789abcdef01234567"

func linuxRecord() EvidenceRecord {
	return EvidenceRecord{
		Commit: evidenceCommit,
		Pins:   []Pin{{Module: "go", Version: "go1.27.4"}, {Module: "gioui.org", Version: "v0.10.3"}},
		Host: Host{
			OS: "linux", Arch: "amd64", Edition: "Ubuntu 24.04.3 LTS", Build: "24.04.3",
			Updates:     []string{"linux-image-6.8.0-88-generic 6.8.0-88.89"},
			ImageDigest: "sha256:" + strings.Repeat("ab", 32),
			Execution:   ExecutionNative,
			Display:     Display{Backend: "wayland"},
			GPU:         "Intel UHD Graphics 620", Driver: "Mesa 25.0.7", Locale: "ko_KR.UTF-8",
			InputMethod: "IBus 1.5.29 with ibus-hangul 1.5.4",
			Linux: &LinuxHost{
				Kernel: "6.8.0-88-generic", Compositor: "GNOME Shell 46.0 (Mutter)",
				DesktopPackages: []string{"gnome-shell 46.0-0ubuntu6"}, PortalBackend: "xdg-desktop-portal-gnome 46.2",
			},
		},
		Checks: []Check{
			{
				Gate: GateClipboard, Kind: CheckAutomated,
				Procedure: "go test -tags gui,guiintegration -run '^TestClipboardRoundTripIsExact$' ./internal/dalgona/gui/platform/",
				Expected:  "the clipboard returns the exact written scalars", Actual: "test passed",
				Outcome: OutcomePassed, Artifact: "evidence/linux-amd64-wayland.log",
			},
			{
				Gate: GateAccessibility, Kind: CheckManualNative, Software: "Orca 46.1",
				Procedure: "complete the core tasks with Orca and no pointer", Expected: "every core task completes",
				Actual: "every core task completed", Outcome: OutcomePassed, Artifact: "evidence/orca.webm",
			},
			{
				Gate: GateDialogs, Kind: CheckManualNative, Procedure: "open the portal file chooser",
				Expected: "the portal dialog opens", Actual: "not run: no portal dialog bridge", Outcome: OutcomeSkipped,
			},
		},
		Missing: []MissingBehavior{{Requirement: "P08", Package: "internal/dalgona/gui/platform", Behavior: "XDG FileChooser portal bridge"}},
	}
}

func TestEvidenceRecordRoundTrips(t *testing.T) {
	var buf bytes.Buffer
	if err := EncodeEvidence(&buf, linuxRecord()); err != nil {
		t.Fatal(err)
	}
	got, err := DecodeEvidence(&buf)
	if err != nil {
		t.Fatal(err)
	}
	if got.Host.Linux.PortalBackend != "xdg-desktop-portal-gnome 46.2" || len(got.Checks) != 3 || got.Checks[2].Outcome != OutcomeSkipped {
		t.Fatalf("decoded record lost facts: %+v", got)
	}
}

func TestEvidenceRecordRequiresTheSpecifiedFacts(t *testing.T) {
	cases := map[string]func(*EvidenceRecord){
		"short commit":               func(r *EvidenceRecord) { r.Commit = "0123456" },
		"uppercase commit":           func(r *EvidenceRecord) { r.Commit = strings.ToUpper(evidenceCommit) },
		"missing Gio pin":            func(r *EvidenceRecord) { r.Pins = r.Pins[:1] },
		"repeated pin":               func(r *EvidenceRecord) { r.Pins = append(r.Pins, r.Pins[0]) },
		"unreleased target":          func(r *EvidenceRecord) { r.Host.Arch = "386" },
		"backend of another OS":      func(r *EvidenceRecord) { r.Host.Display.Backend = "win32" },
		"unknown execution":          func(r *EvidenceRecord) { r.Host.Execution = "simulated" },
		"image without digest":       func(r *EvidenceRecord) { r.Host.ImageDigest = "ubuntu-24.04.iso" },
		"missing edition":            func(r *EvidenceRecord) { r.Host.Edition = "" },
		"missing OS build":           func(r *EvidenceRecord) { r.Host.Build = "" },
		"missing GPU":                func(r *EvidenceRecord) { r.Host.GPU = "" },
		"missing driver":             func(r *EvidenceRecord) { r.Host.Driver = "" },
		"missing locale":             func(r *EvidenceRecord) { r.Host.Locale = "" },
		"missing input method":       func(r *EvidenceRecord) { r.Host.InputMethod = "" },
		"no update list":             func(r *EvidenceRecord) { r.Host.Updates = nil },
		"Linux facts absent":         func(r *EvidenceRecord) { r.Host.Linux = nil },
		"missing kernel":             func(r *EvidenceRecord) { r.Host.Linux.Kernel = "" },
		"missing compositor":         func(r *EvidenceRecord) { r.Host.Linux.Compositor = "" },
		"missing portal backend":     func(r *EvidenceRecord) { r.Host.Linux.PortalBackend = "" },
		"missing desktop packages":   func(r *EvidenceRecord) { r.Host.Linux.DesktopPackages = nil },
		"unknown gate":               func(r *EvidenceRecord) { r.Checks[0].Gate = "smoke" },
		"unknown kind":               func(r *EvidenceRecord) { r.Checks[0].Kind = "simulated" },
		"unknown outcome":            func(r *EvidenceRecord) { r.Checks[0].Outcome = "flaky" },
		"missing procedure":          func(r *EvidenceRecord) { r.Checks[0].Procedure = "" },
		"missing expected result":    func(r *EvidenceRecord) { r.Checks[0].Expected = "" },
		"missing actual result":      func(r *EvidenceRecord) { r.Checks[2].Actual = "" },
		"pass without artifact":      func(r *EvidenceRecord) { r.Checks[0].Artifact = "" },
		"cross-compiled pass":        func(r *EvidenceRecord) { r.Host.Execution = ExecutionCrossCompiled },
		"emulated pass":              func(r *EvidenceRecord) { r.Host.Execution = ExecutionEmulated },
		"snapshot accessibility":     func(r *EvidenceRecord) { r.Checks[1].Kind = CheckAutomated },
		"no assistive software":      func(r *EvidenceRecord) { r.Checks[1].Software = "" },
		"IME pass without engine":    func(r *EvidenceRecord) { r.Checks[1].Gate, r.Checks[1].Software = GateIME, "" },
		"unknown requirement":        func(r *EvidenceRecord) { r.Missing[0].Requirement = "P16" },
		"requirement zero":           func(r *EvidenceRecord) { r.Missing[0].Requirement = "P00" },
		"package outside the module": func(r *EvidenceRecord) { r.Missing[0].Package = "gioui.org/x/explorer" },
		"missing behavior text":      func(r *EvidenceRecord) { r.Missing[0].Behavior = "" },
	}
	for name, breakRecord := range cases {
		rec := linuxRecord()
		breakRecord(&rec)
		if err := rec.Validate(); err == nil {
			t.Errorf("%s: the record was accepted", name)
		}
		if err := EncodeEvidence(new(bytes.Buffer), rec); err == nil {
			t.Errorf("%s: the record was written", name)
		}
	}
}

func TestEvidenceRecordKeepsPlatformFactsApart(t *testing.T) {
	rec := linuxRecord()
	rec.Host.OS, rec.Host.Arch, rec.Host.Display.Backend = "windows", "arm64", "win32"
	if err := rec.Validate(); err == nil {
		t.Fatal("a Windows record accepted Linux facts")
	}
	rec.Host.Linux = nil
	if err := rec.Validate(); err != nil {
		t.Fatalf("a complete Windows record was refused: %v", err)
	}
	failing := linuxRecord()
	failing.Host.Execution = ExecutionCrossCompiled
	for i := range failing.Checks {
		failing.Checks[i].Outcome = OutcomeSkipped
	}
	if err := failing.Validate(); err != nil {
		t.Fatalf("a cross-compiled record with skipped native checks was refused: %v", err)
	}
}

func TestDecodeEvidenceRejectsTamperedRecords(t *testing.T) {
	var buf bytes.Buffer
	if err := EncodeEvidence(&buf, linuxRecord()); err != nil {
		t.Fatal(err)
	}
	tampered := map[string]string{
		"unknown member":      strings.Replace(buf.String(), `"commit"`, `"verified": true, "commit"`, 1),
		"emulated pass":       strings.Replace(buf.String(), `"execution": "native"`, `"execution": "emulated"`, 1),
		"missing Linux facts": strings.Replace(buf.String(), `"kernel": "6.8.0-88-generic"`, `"kernel": ""`, 1),
	}
	for name, record := range tampered {
		if record == buf.String() {
			t.Fatalf("%s: the tampering found no member to change", name)
		}
		if rec, err := DecodeEvidence(strings.NewReader(record)); err == nil || rec.Commit != "" {
			t.Errorf("%s: decoded %q with error %v, want a refusal and no record", name, rec.Commit, err)
		}
	}
}
