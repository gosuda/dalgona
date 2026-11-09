//go:build !windows

package codexapp

func registryValue() (string, bool, error) { return "", false, nil }

func setRegistry(string) error { return nil }

func restoreRegistry(string, bool) error { return nil }
