package client

import (
	"context"
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type MachineMismatchError struct {
	Expected string
	Actual   string
}

func (e *MachineMismatchError) Error() string {
	return fmt.Sprintf("machine identity changed (expected %s, received %s); verify the host and add it as a new endpoint", e.Expected, e.Actual)
}

func machineReplyError(expected string, reply rpc.MachineReply) error {
	switch reply.Status {
	case "":
		return nil
	case rpc.MachineMismatch:
		return &MachineMismatchError{Expected: expected, Actual: reply.MachineID}
	case rpc.MachineDenied:
		return errors.New("pairing credential expired, was already used, or was revoked")
	case rpc.MachineMissing:
		return errors.New("paired device does not exist")
	case rpc.MachineInvalid:
		return errors.New("daemon refused the machine request; check the pairing details and versions")
	default:
		return errors.New("daemon pairing is unavailable; check the device registry and capacity")
	}
}

func (s *EndpointSet) OnboardSSH(ctx context.Context, name string, target SSHTarget) error {
	return s.addMachine(ctx, name, func(ctx context.Context) (config.RemoteSpec, error) { return s.remotes.OnboardSSH(ctx, name, target) })
}

func (s *EndpointSet) PairDevice(ctx context.Context, name string, card rpc.PairingCard, code, deviceLabel string) error {
	return s.addMachine(ctx, name, func(ctx context.Context) (config.RemoteSpec, error) {
		return s.remotes.PairDevice(ctx, name, card, code, deviceLabel)
	})
}

func (s *EndpointSet) addMachine(ctx context.Context, name string, enroll func(context.Context) (config.RemoteSpec, error)) error {
	s.saving.Lock()
	defer s.saving.Unlock()
	if err := s.admit(EndpointID(name)); err != nil {
		return err
	}
	s.mu.Lock()
	_, exists := s.byID[EndpointID(name)]
	s.mu.Unlock()
	if exists {
		return fmt.Errorf("remote: %q already exists; select it or add the machine under a new name", name)
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	stop := context.AfterFunc(s.ctx, cancel)
	defer stop()
	spec, err := enroll(ctx)
	if err != nil {
		return err
	}
	return s.recordRemote(name, spec)
}

func (r *Remotes) addMachine(ctx context.Context, name string, enroll func() (config.RemoteSpec, error)) (config.RemoteSpec, error) {
	if !remoteNamePattern.MatchString(name) {
		return config.RemoteSpec{}, fmt.Errorf("remote: name %q must use only letters, digits, - and _", name)
	}
	if _, exists := r.configuredRemote(name); exists {
		return config.RemoteSpec{}, fmt.Errorf("remote: %q already exists; choose a new name", name)
	}
	if err := ctx.Err(); err != nil {
		return config.RemoteSpec{}, err
	}
	if _, err := r.machineSlot(name); err != nil {
		return config.RemoteSpec{}, err
	}
	spec, err := enroll()
	if err != nil {
		return config.RemoteSpec{}, err
	}
	if err := config.CheckRemote(name, spec); err != nil {
		return config.RemoteSpec{}, err
	}
	if err := ctx.Err(); err != nil {
		return config.RemoteSpec{}, err
	}
	return spec, r.publishMachine(name, spec)
}

func (r *Remotes) machineSlot(name string) (string, error) {
	spec, revision, err := config.LoadRemoteSnapshot(r.home, name)
	if err == nil && spec != (config.RemoteSpec{}) {
		err = fmt.Errorf("remote: %q already exists; choose a new name", name)
	}
	return revision, err
}

func (r *Remotes) publishMachine(name string, spec config.RemoteSpec) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, exists := r.remotes[name]; exists {
		return fmt.Errorf("remote: %q already exists; choose a new name", name)
	}
	revision, err := r.machineSlot(name)
	if err != nil {
		return err
	}
	if _, err := config.WriteSettingsIf(r.home, revision, config.SettingEdit{Key: "remotes." + name, Value: remoteTable(spec)}); err != nil {
		return fmt.Errorf("remote: could not publish %q: %w", name, err)
	}
	r.remotes[name] = spec
	return nil
}
