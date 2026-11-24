//go:build darwin

package platform

// #cgo CFLAGS: -fobjc-arc -x objective-c
// #cgo LDFLAGS: -framework AppKit
// #include "accessibility_darwin.h"
import "C"

import (
	"errors"
	"fmt"
	"os"
	"sync"
	"syscall"
	"unsafe"
)

var errAppKitAttach = errors.New("attach the AppKit accessibility bridge: the view or the accessibility tree is missing; pass the view from app.AppKitViewEvent and the window's tree")

type appKitAccessibility struct {
	tree      *AccessibilityTree
	mu        sync.Mutex
	native    C.uintptr_t
	closed    bool
	published *SemanticSnapshot
	encoder   appKitSnapshotEncoder
	speech    []byte
}

func AttachAppKitAccessibility(view uintptr, tree *AccessibilityTree) (detach func(), err error) {
	if view == 0 || tree == nil {
		return nil, errAppKitAttach
	}
	var native C.uintptr_t
	var actions C.int
	if code := C.dalgona_ax_attach(C.uintptr_t(view), &native, &actions); code != 0 {
		return nil, fmt.Errorf("attach the AppKit accessibility bridge: create its action pipe: %w; close unused files, then reopen the window", syscall.Errno(code)) //nolint:gosec // dalgona_ax_attach returns a positive errno value
	}
	intents := os.NewFile(uintptr(actions), "appkit accessibility actions") //nolint:gosec // pipe(2) returned a non-negative descriptor
	a := &appKitAccessibility{tree: tree, native: native}
	stop := tree.Observe(a)
	served := make(chan struct{})
	go func() {
		defer close(served)
		if serveErr := serveAppKitIntents(intents, tree, a.refuse); serveErr != nil {
			a.refuse(serveErr)
		}
		_ = intents.Close()
	}()
	a.SnapshotChanged(nil, tree.Snapshot())
	var once sync.Once
	return func() {
		once.Do(func() {
			stop()
			a.close()
			<-served
		})
	}, nil
}

func (a *appKitAccessibility) SnapshotChanged(_, _ *SemanticSnapshot) {
	a.mu.Lock()
	defer a.mu.Unlock()
	latest := a.tree.Snapshot()
	if a.closed || latest == nil || latest == a.published {
		return
	}
	snapshot, err := a.encoder.encode(latest)
	if err != nil {
		a.announceLocked(err.Error(), appKitPriorityHigh)
		return
	}
	a.published = latest
	C.dalgona_ax_publish(a.native, (*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(snapshot))), C.size_t(len(snapshot))) //nolint:gosec // dalgona_ax_publish copies the snapshot bytes before it returns
}

func (a *appKitAccessibility) Announced(n Announcement) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.announceLocked(n.Text, announcementPriority(n.Kind))
}

func (a *appKitAccessibility) refuse(err error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.announceLocked(err.Error(), appKitPriorityHigh)
}

func (a *appKitAccessibility) announceLocked(text string, priority appKitPriority) {
	if a.closed || text == "" {
		return
	}
	a.speech = appendAppKitText(a.speech[:0], text)
	C.dalgona_ax_announce(a.native, (*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(a.speech))), C.size_t(len(a.speech)), C.uint8_t(priority)) //nolint:gosec // dalgona_ax_announce copies the UTF-16 text before it returns
}

func (a *appKitAccessibility) close() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.closed = true
	C.dalgona_ax_detach(a.native)
}
