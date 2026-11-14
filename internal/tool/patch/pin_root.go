package patch

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/tool"
)

type rootPin struct {
	root    *os.Root
	missing []string
}

type pinHolder interface {
	openChain(components []string, display string) error
	checkLeaf(name, display string) error
	close()
}

func pinRoot(env tool.Env, display, absolute string) (*rootPin, error) {
	if !tool.IsLocalRunner(env.Runner) {
		return nil, nil
	}
	root, err := os.OpenRoot(env.Cwd)
	if err != nil {
		return nil, fmt.Errorf("patch: cannot hold %s: %w", env.Cwd, err)
	}
	return holdPath(env, display, absolute, &rootPin{root: root})
}

func holdPath[P pinHolder](env tool.Env, display, absolute string, pin P) (P, error) {
	var none P
	fail := func(err error) (P, error) {
		pin.close()
		return none, err
	}
	rel, err := filepath.Rel(env.Cwd, absolute)
	if err != nil {
		return fail(fmt.Errorf("patch: resolve %s: %w", display, err))
	}
	dir, name := filepath.Split(filepath.ToSlash(rel))
	if dir != "" {
		if err := pin.openChain(strings.Split(strings.TrimSuffix(dir, "/"), "/"), display); err != nil {
			return fail(err)
		}
	}
	if err := pin.checkLeaf(name, display); err != nil {
		return fail(err)
	}
	return pin, nil
}

func symlinkRefusal(display string) error {
	return fmt.Errorf("patch: %s contains a symbolic link; patch does not follow links", display)
}

func (p *rootPin) descend(component, display string) error {
	next, err := p.root.OpenRoot(component)
	if err != nil {
		return fmt.Errorf("patch: inspect path %s: %w", display, err)
	}
	_ = p.root.Close()
	p.root = next
	return nil
}

func (p *rootPin) openChain(components []string, display string) error {
	for i, component := range components {
		info, err := p.root.Lstat(component)
		if errors.Is(err, fs.ErrNotExist) {
			p.missing = components[i:]
			return nil
		}
		if err != nil {
			return fmt.Errorf("patch: inspect path %s: %w", display, err)
		}
		if info.Mode()&fs.ModeSymlink != 0 {
			return symlinkRefusal(display)
		}
		if err := p.descend(component, display); err != nil {
			return err
		}
	}
	return nil
}

func (p *rootPin) checkLeaf(name, display string) error {
	if len(p.missing) > 0 {
		return nil
	}
	info, err := p.root.Lstat(name)
	if err == nil {
		if info.Mode()&fs.ModeSymlink != 0 {
			return symlinkRefusal(display)
		}
		return nil
	}
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	return fmt.Errorf("patch: inspect path %s: %w", display, err)
}

func (p *rootPin) mkdirChain(display string, perm fs.FileMode) error {
	if p == nil {
		return nil
	}
	for _, component := range p.missing {
		if err := p.root.Mkdir(component, perm); err != nil && !errors.Is(err, fs.ErrExist) {
			return fmt.Errorf("patch: inspect path %s: %w", display, err)
		}
		info, err := p.root.Lstat(component)
		if err != nil {
			return fmt.Errorf("patch: inspect path %s: %w", display, err)
		}
		if info.Mode()&fs.ModeSymlink != 0 {
			return symlinkRefusal(display)
		}
		if err := p.descend(component, display); err != nil {
			return err
		}
	}
	p.missing = nil
	return nil
}

func (p *rootPin) writeFileAt(base string, data []byte, perm fs.FileMode) error {
	file, err := p.root.OpenFile(base, os.O_WRONLY|os.O_CREATE|os.O_EXCL, perm.Perm())
	if err != nil {
		return fmt.Errorf("patch: create %s: %w", base, err)
	}
	if _, err := file.Write(data); err != nil {
		_ = file.Close()
		return fmt.Errorf("patch: write %s: %w", base, err)
	}
	if err := file.Sync(); err != nil {
		_ = file.Close()
		return fmt.Errorf("patch: sync %s: %w", base, err)
	}
	if err := file.Chmod(perm); err != nil {
		_ = file.Close()
		return fmt.Errorf("patch: set mode on %s: %w", base, err)
	}
	return file.Close()
}

func (p *rootPin) readFileAt(base string) ([]byte, error) {
	info, err := p.root.Lstat(base)
	if err != nil {
		return nil, fmt.Errorf("patch: read %s: %w", base, err)
	}
	if info.Mode()&fs.ModeSymlink != 0 {
		return nil, symlinkRefusal(base)
	}
	data, err := p.root.ReadFile(base)
	if err != nil {
		return nil, fmt.Errorf("patch: read %s: %w", base, err)
	}
	return data, nil
}

func (p *rootPin) leafExists(base string) (bool, error) {
	_, err := p.root.Lstat(base)
	if err == nil {
		return true, nil
	}
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	return false, fmt.Errorf("patch: inspect path %s: %w", base, err)
}

func (p *rootPin) leafExistsAfterChain(base, display string) (bool, error) {
	clone, err := p.root.OpenRoot(".")
	if err != nil {
		return false, fmt.Errorf("patch: inspect path %s: %w", display, err)
	}
	pin := &rootPin{root: clone}
	defer pin.close()
	if err := pin.openChain(p.missing, display); err != nil {
		return false, err
	}
	if len(pin.missing) > 0 {
		return false, nil
	}
	return pin.leafExists(base)
}

type pinFileOps interface {
	writeFileAt(base string, data []byte, perm fs.FileMode) error
	renameFileAt(oldBase, newBase string) error
	unlinkFileAt(base string) error
}

func publishOverPin(p pinFileOps, base string, data []byte, perm fs.FileMode) error {
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

func (p *rootPin) publishOver(base string, data []byte, perm fs.FileMode) error {
	return publishOverPin(p, base, data, perm)
}

func (p *rootPin) renameFileAt(oldBase, newBase string) error {
	if err := replaceInRoot(p.root, oldBase, newBase); err != nil {
		return fmt.Errorf("patch: rename %s: %w", newBase, err)
	}
	return nil
}

func (p *rootPin) linkFileAt(oldBase, newBase string) error {
	if err := p.root.Link(oldBase, newBase); err != nil {
		return fmt.Errorf("patch: link %s: %w", newBase, err)
	}
	return nil
}

func (p *rootPin) unlinkFileAt(base string) error {
	if err := p.root.Remove(base); err != nil {
		return fmt.Errorf("patch: remove %s: %w", base, err)
	}
	return nil
}

func (p *rootPin) close() {
	if p == nil || p.root == nil {
		return
	}
	_ = p.root.Close()
	p.root = nil
}
