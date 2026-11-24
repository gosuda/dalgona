//go:build linux

package platform

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
)

type fakeSettings struct {
	values map[string]any
	legacy bool
}

func (f fakeSettings) lookup(namespace, key string) (any, *dbus.Error) {
	v, ok := f.values[namespace+" "+key]
	if !ok {
		return nil, dbus.NewError("org.freedesktop.portal.Error.NotFound", []any{"Requested setting not found"})
	}
	return v, nil
}

func (f fakeSettings) ReadOne(namespace, key string) (dbus.Variant, *dbus.Error) {
	if f.legacy {
		return dbus.Variant{}, dbus.NewError("org.freedesktop.DBus.Error.UnknownMethod", []any{"ReadOne"})
	}
	v, err := f.lookup(namespace, key)
	if err != nil {
		return dbus.Variant{}, err
	}
	return dbus.MakeVariant(v), nil
}

func (f fakeSettings) Read(namespace, key string) (dbus.Variant, *dbus.Error) {
	v, err := f.lookup(namespace, key)
	if err != nil {
		return dbus.Variant{}, err
	}
	return dbus.MakeVariant(dbus.MakeVariant(v)), nil
}

func readFakePreference(t *testing.T, settings fakeSettings, p desktopPreference) (bool, error) {
	t.Helper()
	address := startPortalTestBus(t)
	server := connectPortalTestBus(t, address)
	if err := server.Export(settings, portalObject, portalSettings); err != nil {
		t.Fatal(err)
	}
	if _, err := server.RequestName(portalService, dbus.NameFlagDoNotQueue); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	return readPreference(ctx, connectPortalTestBus(t, address), p)
}

func TestPreferencesFollowTheAppearanceNamespace(t *testing.T) {
	cases := []struct {
		name     string
		settings fakeSettings
		p        desktopPreference
		want     bool
	}{
		{"reduced motion requested", fakeSettings{values: map[string]any{"org.freedesktop.appearance reduced-motion": uint32(1)}}, reducedMotionPreference, true},
		{"no motion preference", fakeSettings{values: map[string]any{"org.freedesktop.appearance reduced-motion": uint32(0)}}, reducedMotionPreference, false},
		{"GNOME animations off", fakeSettings{values: map[string]any{"org.gnome.desktop.interface enable-animations": false}}, reducedMotionPreference, true},
		{"GNOME animations on", fakeSettings{values: map[string]any{"org.gnome.desktop.interface enable-animations": true}}, reducedMotionPreference, false},
		{"legacy portal Read", fakeSettings{legacy: true, values: map[string]any{"org.freedesktop.appearance reduced-motion": uint32(1)}}, reducedMotionPreference, true},
		{"higher contrast", fakeSettings{values: map[string]any{"org.freedesktop.appearance contrast": uint32(1)}}, highContrastPreference, true},
		{"GNOME high contrast", fakeSettings{values: map[string]any{"org.gnome.desktop.a11y.interface high-contrast": true}}, highContrastPreference, true},
		{"dark requested", fakeSettings{values: map[string]any{"org.freedesktop.appearance color-scheme": uint32(1)}}, darkPreference, true},
		{"light requested", fakeSettings{values: map[string]any{"org.freedesktop.appearance color-scheme": uint32(2)}}, darkPreference, false},
		{"no colour preference", fakeSettings{values: map[string]any{"org.freedesktop.appearance color-scheme": uint32(0)}}, darkPreference, false},
		{"GNOME prefers dark", fakeSettings{values: map[string]any{"org.gnome.desktop.interface color-scheme": "prefer-dark"}}, darkPreference, true},
		{"GNOME default scheme", fakeSettings{values: map[string]any{"org.gnome.desktop.interface color-scheme": "default"}}, darkPreference, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := readFakePreference(t, tc.settings, tc.p)
			if err != nil || got != tc.want {
				t.Fatalf("preference %v, error %v; want %v", got, err, tc.want)
			}
		})
	}
}

func TestPreferencesReportAnUnknownSetting(t *testing.T) {
	if got, err := readFakePreference(t, fakeSettings{values: map[string]any{}}, reducedMotionPreference); err == nil {
		t.Fatalf("a missing setting read as %v without an error", got)
	}
}

func settingSignal(namespace, key string, value any) *dbus.Signal {
	return &dbus.Signal{Path: portalObject, Name: portalSettings + "." + settingChanged, Body: []any{namespace, key, dbus.MakeVariant(value)}}
}

func TestOnlyAppearanceSettingsCountAsAppearanceChanges(t *testing.T) {
	cases := []struct {
		name   string
		signal *dbus.Signal
		want   bool
	}{
		{"portal colour scheme", settingSignal(appearanceNamespace, "color-scheme", uint32(1)), true},
		{"portal contrast", settingSignal(appearanceNamespace, "contrast", uint32(1)), true},
		{"portal reduced motion", settingSignal(appearanceNamespace, "reduced-motion", uint32(1)), true},
		{"GNOME colour scheme", settingSignal("org.gnome.desktop.interface", "color-scheme", "prefer-dark"), true},
		{"GNOME animations", settingSignal("org.gnome.desktop.interface", "enable-animations", false), true},
		{"GNOME high contrast", settingSignal("org.gnome.desktop.a11y.interface", "high-contrast", true), true},
		{"portal accent colour", settingSignal(appearanceNamespace, "accent-color", uint32(1)), false},
		{"GNOME font", settingSignal("org.gnome.desktop.interface", "font-name", "Sans 11"), false},
		{"another member", &dbus.Signal{Name: portalSettings + ".Other", Body: []any{appearanceNamespace, "color-scheme"}}, false},
		{"a short body", &dbus.Signal{Name: portalSettings + "." + settingChanged, Body: []any{appearanceNamespace}}, false},
	}
	for _, tc := range cases {
		if got := appearanceSignal(tc.signal); got != tc.want {
			t.Errorf("%s counts as an appearance change: %v, want %v", tc.name, got, tc.want)
		}
	}
}

func TestWatchAppearanceRelaysThePortalSignalUntilCancelled(t *testing.T) {
	address := startPortalTestBus(t)
	server := connectPortalTestBus(t, address)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	signals, err := settingSignals(ctx, connectPortalTestBus(t, address))
	if err != nil {
		t.Fatal(err)
	}
	watch, stop := context.WithCancel(ctx)
	changes, done := make(chan struct{}, 1), make(chan error, 1)
	go func() { done <- relayAppearance(watch, signals, func() { changes <- struct{}{} }) }()
	if err := server.Emit(portalObject, portalSettings+"."+settingChanged, appearanceNamespace, "color-scheme", dbus.MakeVariant(uint32(1))); err != nil {
		t.Fatal(err)
	}
	select {
	case <-changes:
	case <-ctx.Done():
		t.Fatal("the colour scheme signal never reported a change")
	}
	stop()
	if err := <-done; err != nil {
		t.Fatalf("a cancelled watch returned %v", err)
	}
}

func TestWatchAppearanceReportsALostSessionBus(t *testing.T) {
	address := startPortalTestBus(t)
	conn := connectPortalTestBus(t, address)
	signals, err := settingSignals(t.Context(), conn)
	if err != nil {
		t.Fatal(err)
	}
	_ = conn.Close()
	if err := relayAppearance(t.Context(), signals, func() {}); !errors.Is(err, errSettingsClosed) {
		t.Fatalf("a closed bus returned %v, want %v", err, errSettingsClosed)
	}
}
