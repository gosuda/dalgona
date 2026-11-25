package platform

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"slices"
	"strings"
)

type Gate string

const (
	GateHandleLifecycle   Gate = "handle-lifecycle"
	GateRendering         Gate = "rendering"
	GateIME               Gate = "ime"
	GateClipboard         Gate = "clipboard"
	GateDialogs           Gate = "dialogs"
	GateMenus             Gate = "menus"
	GateAccessibility     Gate = "accessibility"
	GateTextAccessibility Gate = "text-accessibility"
	GateTerminal          Gate = "terminal"
	GatePackaging         Gate = "packaging"
)

var evidenceGates = []Gate{
	GateHandleLifecycle, GateRendering, GateIME, GateClipboard, GateDialogs,
	GateMenus, GateAccessibility, GateTextAccessibility, GateTerminal, GatePackaging,
}

type CheckKind string

const (
	CheckAutomated    CheckKind = "automated"
	CheckInspection   CheckKind = "inspection"
	CheckManualNative CheckKind = "manual-native"
	CheckLiveService  CheckKind = "live-service"
)

type Outcome string

const (
	OutcomePassed  Outcome = "passed"
	OutcomeFailed  Outcome = "failed"
	OutcomeSkipped Outcome = "skipped"
)

type Execution string

const (
	ExecutionNative        Execution = "native"
	ExecutionEmulated      Execution = "emulated"
	ExecutionCrossCompiled Execution = "cross-compiled"
)

type EvidenceRecord struct {
	Commit  string            `json:"commit"`
	Pins    []Pin             `json:"pins"`
	Host    Host              `json:"host"`
	Checks  []Check           `json:"checks"`
	Missing []MissingBehavior `json:"missing"`
}

type Pin struct {
	Module  string `json:"module"`
	Version string `json:"version"`
}

type Host struct {
	OS          string     `json:"os"`
	Arch        string     `json:"arch"`
	Edition     string     `json:"edition"`
	Build       string     `json:"build"`
	Updates     []string   `json:"updates"`
	ImageDigest string     `json:"image_digest"`
	Execution   Execution  `json:"execution"`
	Display     Display    `json:"display"`
	GPU         string     `json:"gpu"`
	Driver      string     `json:"driver"`
	Locale      string     `json:"locale"`
	InputMethod string     `json:"input_method"`
	Linux       *LinuxHost `json:"linux,omitzero"`
}

type Display struct {
	Backend string `json:"backend"`
	Virtual string `json:"virtual,omitzero"`
}

type LinuxHost struct {
	Kernel          string   `json:"kernel"`
	Compositor      string   `json:"compositor"`
	DesktopPackages []string `json:"desktop_packages"`
	PortalBackend   string   `json:"portal_backend"`
}

type Check struct {
	Gate      Gate      `json:"gate"`
	Kind      CheckKind `json:"kind"`
	Procedure string    `json:"procedure"`
	Expected  string    `json:"expected"`
	Actual    string    `json:"actual"`
	Outcome   Outcome   `json:"outcome"`
	Artifact  string    `json:"artifact"`
	Software  string    `json:"software,omitzero"`
}

type MissingBehavior struct {
	Requirement string `json:"requirement"`
	Package     string `json:"package"`
	Behavior    string `json:"behavior"`
}

var releaseBackends = map[string][]string{
	"linux/amd64":   {"x11", "wayland"},
	"linux/arm64":   {"x11", "wayland"},
	"darwin/amd64":  {"appkit"},
	"darwin/arm64":  {"appkit"},
	"windows/amd64": {"win32"},
	"windows/arm64": {"win32"},
}

var requiredPins = []string{"go", "gioui.org"}

func DecodeEvidence(r io.Reader) (EvidenceRecord, error) {
	var rec EvidenceRecord
	if err := json.UnmarshalRead(r, &rec, json.RejectUnknownMembers(true)); err != nil {
		return EvidenceRecord{}, fmt.Errorf("decode the platform evidence record: %w", err)
	}
	if err := rec.Validate(); err != nil {
		return EvidenceRecord{}, err
	}
	return rec, nil
}

func EncodeEvidence(w io.Writer, rec EvidenceRecord) error {
	if err := rec.Validate(); err != nil {
		return err
	}
	return json.MarshalWrite(w, rec, jsontext.WithIndent("  "))
}

func (rec EvidenceRecord) Validate() error {
	if !isHex(rec.Commit, 40) && !isHex(rec.Commit, 64) {
		return errors.New("evidence commit: record the full 40 or 64 digit lowercase commit hash")
	}
	if err := checkPins(rec.Pins); err != nil {
		return err
	}
	if err := rec.Host.validate(); err != nil {
		return err
	}
	for i, c := range rec.Checks {
		if err := c.validate(rec.Host.Execution); err != nil {
			return fmt.Errorf("evidence check %d (%s): %w", i, c.Gate, err)
		}
	}
	for i, m := range rec.Missing {
		if err := m.validate(); err != nil {
			return fmt.Errorf("evidence missing behavior %d: %w", i, err)
		}
	}
	return nil
}

func checkPins(pins []Pin) error {
	seen := make(map[string]bool, len(pins))
	for _, p := range pins {
		if p.Module == "" || p.Version == "" || seen[p.Module] {
			return fmt.Errorf("evidence pin %q: name each module once with its exact version", p.Module)
		}
		seen[p.Module] = true
	}
	for _, module := range requiredPins {
		if !seen[module] {
			return fmt.Errorf("evidence pins: record the %s pin", module)
		}
	}
	return nil
}

func (h Host) validate() error {
	if err := h.validateTarget(); err != nil {
		return err
	}
	if !strings.HasPrefix(h.ImageDigest, "sha256:") || !isHex(strings.TrimPrefix(h.ImageDigest, "sha256:"), 64) {
		return errors.New("evidence host: record the installation image as sha256:<64 lowercase hex digits>")
	}
	if name := firstMissing([]fact{
		{"edition", h.Edition},
		{"OS build", h.Build},
		{"GPU", h.GPU},
		{"graphics driver", h.Driver},
		{"locale", h.Locale},
		{"input method", h.InputMethod},
	}); name != "" {
		return fmt.Errorf("evidence host: record the %s", name)
	}
	if !listed(h.Updates) {
		return errors.New("evidence host: list every installed update")
	}
	return h.validateLinux()
}

func (h Host) validateTarget() error {
	backends, ok := releaseBackends[h.OS+"/"+h.Arch]
	if !ok {
		return fmt.Errorf("evidence host: %s/%s is not a release target", h.OS, h.Arch)
	}
	if !slices.Contains(backends, h.Display.Backend) {
		return fmt.Errorf("evidence host: display backend %q does not run on %s", h.Display.Backend, h.OS)
	}
	if !slices.Contains([]Execution{ExecutionNative, ExecutionEmulated, ExecutionCrossCompiled}, h.Execution) {
		return fmt.Errorf("evidence host: execution %q is not native, emulated, or cross-compiled", h.Execution)
	}
	return nil
}

func (h Host) validateLinux() error {
	if h.OS != "linux" {
		if h.Linux != nil {
			return errors.New("evidence host: Linux facts belong only to a Linux record")
		}
		return nil
	}
	if h.Linux == nil {
		return errors.New("evidence host: record the kernel, compositor, desktop packages, and portal backend")
	}
	if name := firstMissing([]fact{
		{"kernel", h.Linux.Kernel}, {"compositor", h.Linux.Compositor}, {"portal backend", h.Linux.PortalBackend},
	}); name != "" {
		return fmt.Errorf("evidence host: record the Linux %s", name)
	}
	if !listed(h.Linux.DesktopPackages) {
		return errors.New("evidence host: list every Linux desktop package")
	}
	return nil
}

func (c Check) validate(execution Execution) error {
	switch {
	case !slices.Contains(evidenceGates, c.Gate):
		return errors.New("the gate is not a P15 gate")
	case !slices.Contains([]CheckKind{CheckAutomated, CheckInspection, CheckManualNative, CheckLiveService}, c.Kind):
		return fmt.Errorf("kind %q is not automated, inspection, manual-native, or live-service", c.Kind)
	case !slices.Contains([]Outcome{OutcomePassed, OutcomeFailed, OutcomeSkipped}, c.Outcome):
		return fmt.Errorf("outcome %q is not passed, failed, or skipped", c.Outcome)
	case c.Procedure == "" || c.Expected == "" || c.Actual == "":
		return errors.New("record the exact procedure, the expected result, and the actual result")
	}
	return c.validateProof(execution)
}

func (c Check) validateProof(execution Execution) error {
	switch {
	case c.Outcome == OutcomeSkipped:
		return nil
	case c.Artifact == "":
		return errors.New("record the evidence artifact")
	case c.Outcome == OutcomeFailed:
		return nil
	case execution != ExecutionNative:
		return fmt.Errorf("a %s run cannot pass a native gate", execution)
	case c.needsRealSoftware() && (c.Kind != CheckManualNative || c.Software == ""):
		return errors.New("a pass needs a manual native check that names the real input engine or assistive software and its version")
	}
	return nil
}

func (c Check) needsRealSoftware() bool {
	return c.Gate == GateIME || c.Gate == GateAccessibility || c.Gate == GateTextAccessibility
}

func (m MissingBehavior) validate() error {
	if !isRequirement(m.Requirement) {
		return fmt.Errorf("requirement %q is not P01 to P15", m.Requirement)
	}
	if !strings.HasPrefix(m.Package, "internal/") && !strings.HasPrefix(m.Package, "cmd/") {
		return fmt.Errorf("package %q is not an owning module package path", m.Package)
	}
	if m.Behavior == "" {
		return errors.New("describe the missing behavior")
	}
	return nil
}

func isRequirement(id string) bool {
	if len(id) != 3 || id[0] != 'P' || strings.Trim(id[1:], "0123456789") != "" {
		return false
	}
	n := int(id[1]-'0')*10 + int(id[2]-'0')
	return n >= 1 && n <= 15
}

func isHex(s string, n int) bool {
	return len(s) == n && strings.Trim(s, "0123456789abcdef") == ""
}

type fact struct{ name, value string }

func firstMissing(facts []fact) string {
	for _, f := range facts {
		if f.value == "" {
			return f.name
		}
	}
	return ""
}

func listed(list []string) bool {
	return len(list) > 0 && !slices.Contains(list, "")
}
