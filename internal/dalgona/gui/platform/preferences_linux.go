//go:build linux

package platform

import (
	"context"
	"errors"
	"fmt"
	"slices"

	"github.com/godbus/dbus/v5"
)

const (
	portalSettings      = "org.freedesktop.portal.Settings"
	appearanceNamespace = "org.freedesktop.appearance"
	settingChanged      = "SettingChanged"
	settingSignalQueue  = 16
)

var errSettingsClosed = errors.New("the session bus closed while watching the desktop appearance; restart dalgona to follow later changes")

type desktopPreference struct {
	key           string
	gnomeSchema   string
	gnomeKey      string
	gnomeInverted bool
	gnomeWord     string
}

var (
	reducedMotionPreference = desktopPreference{key: "reduced-motion", gnomeSchema: "org.gnome.desktop.interface", gnomeKey: "enable-animations", gnomeInverted: true}
	highContrastPreference  = desktopPreference{key: "contrast", gnomeSchema: "org.gnome.desktop.a11y.interface", gnomeKey: "high-contrast"}
	darkPreference          = desktopPreference{key: "color-scheme", gnomeSchema: "org.gnome.desktop.interface", gnomeKey: "color-scheme", gnomeWord: "prefer-dark"}
	appearancePreferences   = []desktopPreference{reducedMotionPreference, highContrastPreference, darkPreference}
)

func ReducedMotion(ctx context.Context) (bool, error) {
	return readSessionPreference(ctx, reducedMotionPreference)
}

func HighContrast(ctx context.Context) (bool, error) {
	return readSessionPreference(ctx, highContrastPreference)
}

func PrefersDark(ctx context.Context) (bool, error) {
	return readSessionPreference(ctx, darkPreference)
}

func WatchAppearance(ctx context.Context, changed func()) error {
	conn, err := dbus.ConnectSessionBus(dbus.WithContext(ctx))
	if err != nil {
		return fmt.Errorf("watch the desktop appearance: connect to the session bus: %w", err)
	}
	defer conn.Close()
	signals, err := settingSignals(ctx, conn)
	if err != nil {
		return err
	}
	return relayAppearance(ctx, signals, changed)
}

func settingSignals(ctx context.Context, conn *dbus.Conn) (<-chan *dbus.Signal, error) {
	rule := []dbus.MatchOption{dbus.WithMatchObjectPath(portalObject), dbus.WithMatchInterface(portalSettings), dbus.WithMatchMember(settingChanged)}
	if err := conn.AddMatchSignalContext(ctx, rule...); err != nil {
		return nil, fmt.Errorf("watch the desktop appearance: subscribe to the settings portal: %w", err)
	}
	signals := make(chan *dbus.Signal, settingSignalQueue)
	conn.Signal(signals)
	return signals, nil
}

func relayAppearance(ctx context.Context, signals <-chan *dbus.Signal, changed func()) error {
	for {
		select {
		case <-ctx.Done():
			return nil
		case s, ok := <-signals:
			if !ok {
				return errSettingsClosed
			}
			if appearanceSignal(s) {
				changed()
			}
		}
	}
}

func appearanceSignal(s *dbus.Signal) bool {
	if s.Name != portalSettings+"."+settingChanged || len(s.Body) < 2 {
		return false
	}
	namespace, _ := s.Body[0].(string)
	key, _ := s.Body[1].(string)
	return slices.ContainsFunc(appearancePreferences, func(p desktopPreference) bool { return p.watches(namespace, key) })
}

func (p desktopPreference) watches(namespace, key string) bool {
	return namespace == appearanceNamespace && key == p.key || namespace == p.gnomeSchema && key == p.gnomeKey
}

func readSessionPreference(ctx context.Context, p desktopPreference) (bool, error) {
	conn, err := dbus.SessionBus()
	if err != nil {
		return false, fmt.Errorf("read the desktop %s setting: connect to the session bus: %w", p.key, err)
	}
	return readPreference(ctx, conn, p)
}

func readPreference(ctx context.Context, conn *dbus.Conn, p desktopPreference) (bool, error) {
	var level uint32
	appearanceErr := readSetting(ctx, conn, appearanceNamespace, p.key, &level)
	if appearanceErr == nil {
		return level == 1, nil
	}
	enabled, err := p.readGnome(ctx, conn)
	if err != nil {
		return false, fmt.Errorf("read the desktop %s setting from the settings portal: %w", p.key, errors.Join(appearanceErr, err))
	}
	return enabled, nil
}

func (p desktopPreference) readGnome(ctx context.Context, conn *dbus.Conn) (bool, error) {
	if p.gnomeWord != "" {
		var word string
		err := readSetting(ctx, conn, p.gnomeSchema, p.gnomeKey, &word)
		return word == p.gnomeWord, err
	}
	var enabled bool
	err := readSetting(ctx, conn, p.gnomeSchema, p.gnomeKey, &enabled)
	return enabled != p.gnomeInverted, err
}

func readSetting(ctx context.Context, conn *dbus.Conn, namespace, key string, value any) error {
	portal := conn.Object(portalService, portalObject)
	var v dbus.Variant
	if err := portal.CallWithContext(ctx, portalSettings+".ReadOne", 0, namespace, key).Store(&v); err == nil {
		return v.Store(value)
	}
	var legacy dbus.Variant
	if err := portal.CallWithContext(ctx, portalSettings+".Read", 0, namespace, key).Store(&legacy); err != nil {
		return err
	}
	return legacy.Store(value)
}
