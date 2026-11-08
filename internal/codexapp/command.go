package codexapp

import (
	"context"
	"fmt"
	"os/exec"
)

var windowsEnvironment = environmentMechanism{
	name:    "registry",
	current: currentWindowsEnvironment,
	set:     setWindowsEnvironment,
	restore: restoreWindowsEnvironment,
}

func currentWindowsEnvironment(_ Env) (string, bool, error) {
	return registryValue()
}

func setWindowsEnvironment(_ Env, exe string) error {
	return setRegistry(exe)
}

func restoreWindowsEnvironment(_ Env, value string, found bool) error {
	return restoreRegistry(value, found)
}

func runCommand(env Env, name string, args ...string) error {
	_, err := commandOutput(env, name, args...)
	return err
}

func commandOutput(env Env, name string, args ...string) ([]byte, error) {
	switch fn := env.CommandRunner.(type) {
	case nil:
		return exec.Command(name, args...).Output()
	case func(string, ...string) error:
		return nil, fn(name, args...)
	case func(string, ...string) ([]byte, error):
		return fn(name, args...)
	case func(string, ...string) (string, error):
		output, err := fn(name, args...)
		return []byte(output), err
	case func(context.Context, string, ...string) error:
		return nil, fn(context.Background(), name, args...)
	case func(context.Context, string, ...string) ([]byte, error):
		return fn(context.Background(), name, args...)
	default:
		return nil, fmt.Errorf("codexapp: unsupported command runner %T", env.CommandRunner)
	}
}
