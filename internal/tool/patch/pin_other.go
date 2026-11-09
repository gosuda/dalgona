//go:build !unix

package patch

import (
	"errors"
	"io/fs"

	"github.com/gosuda/dalgona/internal/tool"
)

type dirPin struct{}

func pinVerifiedComponents(env tool.Env, display, absolute string) (*dirPin, error) {
	return nil, nil
}

func (p *dirPin) mkdirChain(display string, perm fs.FileMode) error {
	return nil
}

func (p *dirPin) writeFileAt(base string, data []byte, perm fs.FileMode) error {
	return errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) renameFileAt(oldBase, newBase string) error {
	return errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) linkFileAt(oldBase, newBase string) error {
	return errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) unlinkFileAt(base string) error {
	return errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) readFileAt(base string) ([]byte, error) {
	return nil, errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) leafExists(base string) (bool, error) {
	return false, errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) leafExistsAfterChain(base, display string) (bool, error) {
	return false, errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) publishOver(base string, data []byte, perm fs.FileMode) error {
	return errors.New("patch: pinned writes are unavailable on this platform")
}

func (p *dirPin) close() {}
