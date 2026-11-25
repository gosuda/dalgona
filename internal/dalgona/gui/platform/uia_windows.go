//go:build windows && (amd64 || arm64)

package platform

import (
	"errors"
	"fmt"
	"maps"
	"runtime"
	"slices"
	"sync"
	"sync/atomic"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	uiaWMNull      uintptr = 0x0000
	uiaWMDestroy   uintptr = 0x0002
	uiaWMGetObject uintptr = 0x003D
	uiaRootObject  uintptr = 0xFFFFFFE7
)

const (
	uiaChildrenInvalidatedChange uintptr = 2
	uiaNoticeQueue               int     = 32
)

var (
	uiaBridges     sync.Map
	uiaSubclassIDs atomic.Uintptr
)

type uiaBridge struct {
	hwnd     uintptr
	id       uintptr
	tree     atomic.Pointer[AccessibilityTree]
	window   *uiaElement
	hooked   atomic.Bool
	detached atomic.Bool
	changed  chan struct{}
	notices  chan Announcement
	done     chan struct{}
	stop     func()
	once     sync.Once
	mu       sync.Mutex
	elements map[NodeID]*uiaElement
	orphans  []*uiaElement
}

func AttachUIA(hwnd uintptr, tree *AccessibilityTree) (detach func(), err error) {
	if hwnd == 0 || tree == nil {
		return nil, errors.New("attach UI Automation: the window handle or accessibility tree is missing; attach after a valid Win32ViewEvent")
	}
	if err = uiaReady(); err != nil {
		return nil, err
	}
	owner, err := windows.GetWindowThreadProcessId(windows.HWND(hwnd), nil)
	if err != nil {
		return nil, fmt.Errorf("attach UI Automation: the window handle is not a live window (%w); attach with the handle from the current Win32ViewEvent", err)
	}
	if owner != windows.GetCurrentThreadId() {
		return nil, errors.New("attach UI Automation: the call ran off the window thread; call AttachUIA inside app.Window.Run")
	}
	b := &uiaBridge{
		hwnd:     hwnd,
		id:       uiaSubclassIDs.Add(1),
		changed:  make(chan struct{}, 1),
		notices:  make(chan Announcement, uiaNoticeQueue),
		done:     make(chan struct{}),
		elements: make(map[NodeID]*uiaElement),
	}
	b.tree.Store(tree)
	b.window = newUIAElement(b, "", 0)
	uiaAddRef(b.window)
	b.hooked.Store(true)
	uiaBridges.Store(b.id, b)
	if ok, _, _ := uiaProcSetWindowSubclass.Call(hwnd, uiaCallbacks.subclass, b.id, 0); ok == 0 {
		uiaBridges.Delete(b.id)
		uiaRelease(b.window)
		return nil, errors.New("attach UI Automation: SetWindowSubclass refused the window; attach on the thread that created the window")
	}
	b.stop = tree.Observe(b)
	go b.serve(tree.Snapshot())
	return b.detach, nil
}

func (b *uiaBridge) SnapshotChanged(_, _ *SemanticSnapshot) {
	select {
	case b.changed <- struct{}{}:
	default:
	}
}

func (b *uiaBridge) Announced(n Announcement) {
	select {
	case b.notices <- n:
	default:
	}
}

func (b *uiaBridge) detach() {
	b.once.Do(func() {
		b.detached.Store(true)
		b.stop()
		b.tree.Store(nil)
		close(b.done)
		if uiaOnWindowThread(b.hwnd) {
			b.unhook()
			return
		}
		_, _, _ = uiaProcPostMessage.Call(b.hwnd, uiaWMNull, 0, 0)
	})
}

func uiaOnWindowThread(hwnd uintptr) bool {
	owner, err := windows.GetWindowThreadProcessId(windows.HWND(hwnd), nil)
	return err == nil && owner == windows.GetCurrentThreadId()
}

func (b *uiaBridge) unhook() {
	if !b.hooked.CompareAndSwap(true, false) {
		return
	}
	_, _, _ = uiaProcRemoveWindowSubclass.Call(b.hwnd, uiaCallbacks.subclass, b.id)
	_, _, _ = procUiaReturnRawElementProvider.Call(b.hwnd, 0, 0, 0)
	uiaBridges.Delete(b.id)
	uiaRelease(b.window)
}

func uiaSubclassProc(hwnd, message, wParam, lParam, id, _ uintptr) uintptr { //nolint:revive // SUBCLASSPROC passes six parameters
	found, _ := uiaBridges.Load(id)
	b, _ := found.(*uiaBridge)
	switch {
	case b == nil:
	case b.detached.Load() || message == uiaWMDestroy:
		b.unhook()
	case message == uiaWMGetObject && lParam&0xFFFFFFFF == uiaRootObject:
		result, _, _ := procUiaReturnRawElementProvider.Call(hwnd, wParam, lParam, uiaPointer(&b.window.simple))
		return result
	}
	result, _, _ := uiaProcDefSubclassProc.Call(hwnd, message, wParam, lParam)
	return result
}

func (b *uiaBridge) element(id NodeID, role Role) *uiaElement {
	b.mu.Lock()
	defer b.mu.Unlock()
	el := b.elements[id]
	if el != nil && el.role == role {
		return el
	}
	if el != nil {
		el.gone.Store(true)
		b.orphans = append(b.orphans, el)
	}
	el = newUIAElement(b, id, role)
	b.elements[id] = el
	return el
}

func (b *uiaBridge) serve(last *SemanticSnapshot) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if err := windows.CoInitializeEx(0, windows.COINIT_MULTITHREADED); err == nil || errors.Is(err, windows.Errno(windows.S_FALSE)) {
		defer windows.CoUninitialize()
	}
	for {
		select {
		case <-b.done:
			b.disconnectAll()
			return
		case <-b.changed:
			last = b.publish(last)
		case n := <-b.notices:
			b.announce(n)
		}
	}
}

func (b *uiaBridge) publish(last *SemanticSnapshot) *SemanticSnapshot {
	tree := b.tree.Load()
	if tree == nil {
		return last
	}
	next := tree.Snapshot()
	events := uiaChanges(last, next)
	listening, _, _ := procUiaClientsAreListening.Call()
	for _, e := range events {
		switch {
		case e.kind == uiaRetire:
			b.retire(e.node, e.role)
		case listening != 0:
			b.raise(e)
		}
	}
	return next
}

func (b *uiaBridge) provider(e uiaEvent) *uiaElement {
	if e.node == "" {
		return b.window
	}
	return b.element(e.node, e.role)
}

func (b *uiaBridge) raise(e uiaEvent) {
	el := b.provider(e)
	uiaAddRef(el)
	defer uiaRelease(el)
	provider := uiaPointer(&el.simple)
	switch e.kind {
	case uiaRaise:
		_, _, _ = procUiaRaiseAutomationEvent.Call(provider, uiaArg(e.id))
	case uiaPropertyChange:
		uiaRaiseProperty(provider, e)
	case uiaChildrenInvalidated:
		_, _, _ = procUiaRaiseStructureChangedEvent.Call(provider, uiaChildrenInvalidatedChange, 0, 0)
	case uiaRetire:
	}
}

func uiaArg(v int32) uintptr {
	if v < 0 {
		return 0
	}
	return uintptr(v)
}

func uiaRaiseProperty(provider uintptr, e uiaEvent) {
	var before, after uiaVariant
	defer before.clear()
	defer after.clear()
	if uiaFailed(before.set(e.previous)) || uiaFailed(after.set(e.next)) {
		return
	}
	_, _, _ = procUiaRaisePropertyChangedEvent.Call(provider, uiaArg(e.id), uintptr(unsafe.Pointer(&before)), uintptr(unsafe.Pointer(&after))) //nolint:gosec // the 64-bit Windows ABIs pass each 24-byte VARIANT by reference to a caller copy
}

func (b *uiaBridge) announce(n Announcement) {
	listening, _, _ := procUiaClientsAreListening.Call()
	if b.tree.Load() == nil || listening == 0 {
		return
	}
	notice := uiaNotification(n.Kind)
	text, activity := uiaBSTR(n.Text), uiaBSTR(notice.activity)
	defer uiaFreeBSTR(text)
	defer uiaFreeBSTR(activity)
	uiaAddRef(b.window)
	defer uiaRelease(b.window)
	_, _, _ = procUiaRaiseNotificationEvent.Call(uiaPointer(&b.window.simple), uiaArg(notice.kind), uiaArg(notice.processing), text, activity)
}

func uiaFreeBSTR(text uintptr) {
	_, _, _ = uiaProcSysFreeString.Call(text)
}

func (b *uiaBridge) retire(id NodeID, role Role) {
	b.mu.Lock()
	retired := b.orphans
	b.orphans = nil
	if el := b.elements[id]; el != nil && el.role == role {
		delete(b.elements, id)
		retired = append(retired, el)
	}
	b.mu.Unlock()
	uiaDisconnect(retired)
}

func (b *uiaBridge) disconnectAll() {
	b.mu.Lock()
	retired := append(slices.Collect(maps.Values(b.elements)), b.orphans...)
	b.elements, b.orphans = make(map[NodeID]*uiaElement), nil
	b.mu.Unlock()
	uiaDisconnect(append(retired, b.window))
}

func uiaDisconnect(elements []*uiaElement) {
	for _, el := range elements {
		el.gone.Store(true)
		_, _, _ = procUiaDisconnectProvider.Call(uiaPointer(&el.simple))
	}
}
