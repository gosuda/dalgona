package tui

import (
	"errors"
	"fmt"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestErrorNoticesNameRecovery(t *testing.T) {
	t.Parallel()
	bad := notificationMsg{method: "item/started", params: []byte(`[]`)}
	probe := newModel(nil, Options{}, nil)
	_, cause := probe.applyDecodedNotification(bad)
	if cause == nil {
		t.Fatal("the malformed notification decoded")
	}
	for _, tc := range []struct {
		name string
		run  func(t *testing.T) model
		want string
	}{
		{
			name: "held notification that does not decode",
			run: func(t *testing.T) model {
				m := newModel(nil, Options{}, nil)
				m.thread.id = "t1"
				m.replayHeld([]notificationMsg{bad})
				return m
			},
			want: fmt.Sprintf("protocol error in item/started: %v; reconnect with /resume t1 or use /new", cause),
		},
		{
			name: "held notification with no thread yet",
			run: func(t *testing.T) model {
				m := newModel(nil, Options{}, nil)
				m.replayHeld([]notificationMsg{bad})
				return m
			},
			want: fmt.Sprintf("protocol error in item/started: %v; reconnect with /resume or use /new", cause),
		},
		{
			name: "cancel of a started sign-in fails",
			run: func(t *testing.T) model {
				m := startedLoginPanel(t)
				m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
				m, _ = drive(t, m, resultMsg{op: "loginCancel", err: errors.New("connection reset")})
				return m
			},
			want: "cancel sign-in failed: connection reset; run /login again",
		},
		{
			name: "sign-in start fails after Esc",
			run: func(t *testing.T) model {
				m := openLoginPanel(t, new(fakeCaller), "chatgpt")
				m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
				m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
				m, _ = drive(t, m, resultMsg{op: "login", err: errors.New("provider unreachable")})
				return m
			},
			want: "Login failed: provider unreachable; run /login again",
		},
		{
			name: "settings save fails after the panel closed",
			run: func(t *testing.T) model {
				m := openSettingsPanel(t, newTestModel(new(fakeCaller), Options{}), "ask")
				m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
				m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
				m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
				m, _ = drive(t, m, resultMsg{op: "settingsSave", err: errors.New("disk full")})
				return m
			},
			want: "Could not save settings: disk full; run /settings and save again",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := tc.run(t)
			if !m.noticeError {
				t.Fatalf("notice %q is not an error notice", m.notice)
			}
			if m.notice != tc.want {
				t.Fatalf("notice = %q, want %q", m.notice, tc.want)
			}
		})
	}
}

func startedLoginPanel(t *testing.T) model {
	t.Helper()
	m := openLoginPanel(t, new(fakeCaller), "chatgpt")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, resultMsg{op: "login", value: &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: "https://auth.example/start"}})
	return m
}
