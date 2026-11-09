//go:build unix

package patch

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/tool"
	"golang.org/x/sys/unix"
)

type dirPin struct {
	file    *os.File
	missing []string
}

func pinVerifiedComponents(env tool.Env, display, absolute string) (*dirPin, error) {
	if !isLocalRunner(env.Runner) {
		return nil, nil
	}
	root, err := os.Open(env.Cwd)
	if err != nil {
		return nil, fmt.Errorf("patch: cannot hold %s: %w", env.Cwd, err)
	}
	pin := &dirPin{file: root}
	rel, err := filepath.Rel(env.Cwd, absolute)
	if err != nil {
		pin.close()
		return nil, fmt.Errorf("patch: resolve %s: %w", display, err)
	}
	dir, name := filepath.Split(filepath.ToSlash(rel))
	if dir != "" {
		if err := pin.openChain(strings.Split(strings.TrimSuffix(dir, "/"), "/"), display); err != nil {
			pin.close()
			return nil, err
		}
	}
	if err := pin.checkLeaf(name, display); err != nil {
		pin.close()
		return nil, err
	}
	return pin, nil
}

func (p *dirPin) openChain(components []string, display string) error {
	for i, component := range components {
		fd, err := unix.Openat(int(p.file.Fd()), component, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
		if err == nil {
			next := os.NewFile(uintptr(fd), filepath.Join(p.file.Name(), component))
			_ = p.file.Close()
			p.file = next
			continue
		}
		if errors.Is(err, unix.ENOENT) {
			p.missing = components[i:]
			return nil
		}
		if errors.Is(err, unix.ELOOP) {
			return fmt.Errorf("patch: %s contains a symbolic link; patch does not follow links", display)
		}
		return fmt.Errorf("patch: inspect path %s: %w", display, err)
	}
	return nil
}

func (p *dirPin) checkLeaf(name, display string) error {
	var info unix.Stat_t
	err := unix.Fstatat(int(p.file.Fd()), name, &info, unix.AT_SYMLINK_NOFOLLOW)
	if err == nil {
		if info.Mode&unix.S_IFMT == unix.S_IFLNK {
			return fmt.Errorf("patch: %s contains a symbolic link; patch does not follow links", display)
		}
		return nil
	}
	if errors.Is(err, unix.ENOENT) {
		return nil
	}
	return fmt.Errorf("patch: inspect path %s: %w", display, err)
}

func (p *dirPin) mkdirChain(display string, perm fs.FileMode) error {
	if p == nil {
		return nil
	}
	for _, component := range p.missing {
		if err := unix.Mkdirat(int(p.file.Fd()), component, uint32(perm)); err != nil && !errors.Is(err, unix.EEXIST) {
			return fmt.Errorf("patch: inspect path %s: %w", display, err)
		}
		fd, err := unix.Openat(int(p.file.Fd()), component, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
		if err != nil {
			if errors.Is(err, unix.ELOOP) {
				return fmt.Errorf("patch: %s contains a symbolic link; patch does not follow links", display)
			}
			return fmt.Errorf("patch: inspect path %s: %w", display, err)
		}
		next := os.NewFile(uintptr(fd), filepath.Join(p.file.Name(), component))
		_ = p.file.Close()
		p.file = next
	}
	p.missing = nil
	return nil
}

func (p *dirPin) writeFileAt(base string, data []byte, perm fs.FileMode) error {
	fd, err := unix.Openat(int(p.file.Fd()), base, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_CLOEXEC, uint32(perm.Perm()))
	if err != nil {
		return fmt.Errorf("patch: create %s: %w", base, err)
	}
	file := os.NewFile(uintptr(fd), base)
	if err := file.Chmod(perm); err != nil {
		_ = file.Close()
		return fmt.Errorf("patch: set mode on %s: %w", base, err)
	}
	if _, err := file.Write(data); err != nil {
		_ = file.Close()
		return fmt.Errorf("patch: write %s: %w", base, err)
	}
	if err := file.Sync(); err != nil {
		_ = file.Close()
		return fmt.Errorf("patch: sync %s: %w", base, err)
	}
	return file.Close()
}

func (p *dirPin) readFileAt(base string) ([]byte, error) {
	fd, err := unix.Openat(int(p.file.Fd()), base, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, fmt.Errorf("patch: read %s: %w", base, err)
	}
	file := os.NewFile(uintptr(fd), base)
	defer func() { _ = file.Close() }()
	return io.ReadAll(file)
}

func (p *dirPin) leafExists(base string) (bool, error) {
	var info unix.Stat_t
	err := unix.Fstatat(int(p.file.Fd()), base, &info, unix.AT_SYMLINK_NOFOLLOW)
	if err == nil {
		return true, nil
	}
	if errors.Is(err, unix.ENOENT) {
		return false, nil
	}
	return false, fmt.Errorf("patch: inspect path %s: %w", base, err)
}

func (p *dirPin) leafExistsAfterChain(base, display string) (bool, error) {
	dup, err := unix.Dup(int(p.file.Fd()))
	if err != nil {
		return false, fmt.Errorf("patch: inspect path %s: %w", display, err)
	}
	clone := &dirPin{file: os.NewFile(uintptr(dup), p.file.Name())}
	defer clone.close()
	if err := clone.openChain(p.missing, display); err != nil {
		return false, err
	}
	if len(clone.missing) > 0 {
		return false, nil
	}
	return clone.leafExists(base)
}

func (p *dirPin) publishOver(base string, data []byte, perm fs.FileMode) error {
	suffix, err := tempSuffix()
	if err != nil {
		return err
	}
	temp := base + ".dalgona-publish-" + suffix
	if err := p.writeFileAt(temp, data, perm); err != nil {
		_ = p.unlinkFileAt(temp)
		return err
	}
	if err := p.renameFileAt(temp, base); err != nil {
		_ = p.unlinkFileAt(temp)
		return err
	}
	return nil
}

func (p *dirPin) renameFileAt(oldBase, newBase string) error {
	if err := unix.Renameat(int(p.file.Fd()), oldBase, int(p.file.Fd()), newBase); err != nil {
		return fmt.Errorf("patch: rename %s: %w", newBase, err)
	}
	return nil
}

func (p *dirPin) linkFileAt(oldBase, newBase string) error {
	if err := unix.Linkat(int(p.file.Fd()), oldBase, int(p.file.Fd()), newBase, 0); err != nil {
		return fmt.Errorf("patch: link %s: %w", newBase, err)
	}
	return nil
}

func (p *dirPin) unlinkFileAt(base string) error {
	if err := unix.Unlinkat(int(p.file.Fd()), base, 0); err != nil {
		return fmt.Errorf("patch: remove %s: %w", base, err)
	}
	return nil
}

func (p *dirPin) close() {
	if p == nil || p.file == nil {
		return
	}
	_ = p.file.Close()
}
