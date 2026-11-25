//go:build windows && (amd64 || arm64)

package platform

import (
	"context"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"sync/atomic"
	"syscall"
	"unicode/utf16"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	winrtInitMultithreaded           = 1
	winrtQueryInterface              = 0
	winrtRelease                     = 2
	winrtHResultBits                 = 0xFFFFFFFF
	toastManagerCreateNotifierWithID = 7
	toastNotifierShow                = 6
	toastNotifierSetting             = 8
	toastFactoryCreate               = 6
	toastNotification2PutTag         = 6
	toastNotification2PutGroup       = 8
	xmlDocumentIOLoadXML             = 6
	toastDismissedReason             = 6
	toastDismissalTimedOut           = 2
	toastActivated                   = 0
	toastDismissed                   = 1
	toastFailed                      = 2
	toastNotificationsSettings       = "ms-settings:notifications"
)

var (
	winrtCombase                                = windows.NewLazySystemDLL("combase.dll")
	procRoInitialize                            = winrtCombase.NewProc("RoInitialize")
	procRoUninitialize                          = winrtCombase.NewProc("RoUninitialize")
	procRoGetActivationFactory                  = winrtCombase.NewProc("RoGetActivationFactory")
	procRoActivateInstance                      = winrtCombase.NewProc("RoActivateInstance")
	procWindowsCreateString                     = winrtCombase.NewProc("WindowsCreateString")
	procWindowsDeleteString                     = winrtCombase.NewProc("WindowsDeleteString")
	procSetCurrentProcessExplicitAppUserModelID = shell32.NewProc("SetCurrentProcessExplicitAppUserModelID")
	winrtProcedures                             = []*windows.LazyProc{procRoInitialize, procRoUninitialize, procRoGetActivationFactory, procRoActivateInstance, procWindowsCreateString, procWindowsDeleteString, procCoRegisterClassObject, procCoRevokeClassObject}
	iidUnknown                                  = winrtGUID{0x00000000, 0x0000, 0x0000, [8]byte{0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46}}
	iidAgileObject                              = winrtGUID{0x94ea2b94, 0xe9cc, 0x49e0, [8]byte{0xc0, 0xff, 0xee, 0x64, 0xca, 0x8f, 0x5b, 0x90}}
	iidToastNotificationManagerStatics          = winrtGUID{0x50ac103f, 0xd235, 0x4598, [8]byte{0xbb, 0xef, 0x98, 0xfe, 0x4d, 0x1a, 0x3a, 0xd4}}
	iidToastNotificationFactory                 = winrtGUID{0x04124b20, 0x82c6, 0x4229, [8]byte{0xb1, 0x09, 0xfd, 0x9e, 0xd4, 0x66, 0x2b, 0x53}}
	iidToastNotification2                       = winrtGUID{0x9dfb9fd1, 0x143a, 0x490e, [8]byte{0x90, 0xbf, 0xb9, 0xfb, 0xa7, 0x13, 0x2d, 0xe7}}
	iidXMLDocument                              = winrtGUID{0xf7f3a506, 0x1e87, 0x42d6, [8]byte{0xbc, 0xfb, 0xb8, 0xc8, 0x09, 0xfa, 0x54, 0x94}}
	iidXMLDocumentIO                            = winrtGUID{0x6cd0e74e, 0xee65, 0x4489, [8]byte{0x9e, 0xbf, 0xca, 0x43, 0xe8, 0x7b, 0xa6, 0x37}}
	errNotifierClosed                           = errors.New("the Windows notifier is closed; reopen the window to send notifications")
	toastEvents                                 = [...]struct {
		add, remove int
		iid         *winrtGUID
	}{
		toastActivated: {add: 11, remove: 12, iid: &iidToastActivatedHandler},
		toastDismissed: {add: 9, remove: 10, iid: &iidToastDismissedHandler},
		toastFailed:    {add: 13, remove: 14, iid: &iidToastFailedHandler},
	}
	toastHandlerSlots  [4]uintptr
	toastHandlerVtable = sync.OnceFunc(func() {
		toastHandlerSlots = [4]uintptr{
			syscall.NewCallback(toastHandlerQueryInterface),
			syscall.NewCallback(toastHandlerAddRef),
			syscall.NewCallback(toastHandlerRelease),
			syscall.NewCallback(toastHandlerInvoke),
		}
	})
)

type Notifier struct {
	notifierState
	identity error
	clicked  toastActivations
	calls    chan func(*toastSession)
	retire   chan struct{}
	quit     chan struct{}
	stopped  chan struct{}
	closing  sync.Once
	retired  struct {
		sync.Mutex
		toasts []*shownToast
	}
}

type toastSession struct {
	owner     *Notifier
	ready     bool
	listening bool
	activator uint32
	notifier  *winrtObject
	factory   *winrtObject
	shown     map[string]*shownToast
}

type shownToast struct {
	owner    *Notifier
	tag      string
	key      string
	toast    *winrtObject
	handlers [len(toastEvents)]*toastHandler
	tokens   [len(toastEvents)]uintptr
	released bool
}

type toastHandler struct {
	vtbl   *[4]uintptr
	iid    *winrtGUID
	shown  *shownToast
	kind   int
	refs   atomic.Uint32
	pinner runtime.Pinner
}

type winrtObject struct {
	vtbl *[16]uintptr
}

type winrtString struct {
	_ uintptr
}

type toastPermission struct {
	permission NotificationPermission
	err        error
}

func NewNotifier(wake func()) *Notifier {
	n := &Notifier{
		notifierState: newNotifierState(wake),
		calls:         make(chan func(*toastSession)),
		retire:        make(chan struct{}, 1),
		quit:          make(chan struct{}),
		stopped:       make(chan struct{}),
	}
	n.identity = setProcessAppID()
	go n.run()
	return n
}

func (n *Notifier) Request(ctx context.Context) (NotificationPermission, error) {
	return n.Refresh(ctx)
}

func (n *Notifier) Refresh(ctx context.Context) (NotificationPermission, error) {
	if n.identity != nil {
		n.settle(PermissionUnavailable)
		return PermissionUnavailable, n.identity
	}
	result, err := notifierCall(ctx, n, func(s *toastSession) toastPermission {
		permission, err := s.permission()
		n.settle(permission)
		return toastPermission{permission, err}
	})
	if err != nil {
		return n.Permission(), err
	}
	return result.permission, result.err
}

func (n *Notifier) Send(ctx context.Context, note Notification) error {
	if n.Permission() != PermissionGranted {
		return ErrNotificationsOff
	}
	content, err := toastXML(note.Title, note.Body, note.Tag)
	if err != nil {
		return err
	}
	showErr, err := notifierCall(ctx, n, func(s *toastSession) error { return s.show(n, note.Tag, content) })
	if err != nil {
		return err
	}
	return showErr
}

func (n *Notifier) OpenSettings() error {
	return shellOpen(context.Background(), toastNotificationsSettings)
}

func (n *Notifier) Close() error {
	n.closing.Do(func() { close(n.quit) })
	<-n.stopped
	return nil
}

func notifierCall[T any](ctx context.Context, n *Notifier, work func(*toastSession) T) (T, error) {
	var zero T
	done := make(chan T, 1)
	select {
	case n.calls <- func(s *toastSession) { done <- work(s) }:
	case <-ctx.Done():
		return zero, context.Cause(ctx)
	case <-n.stopped:
		return zero, errNotifierClosed
	}
	select {
	case result := <-done:
		return result, nil
	case <-ctx.Done():
		return zero, context.Cause(ctx)
	}
}

func (n *Notifier) run() {
	runtime.LockOSThread()
	defer close(n.stopped)
	session := toastSession{owner: n, shown: make(map[string]*shownToast)}
	defer session.close()
	_ = session.start()
	for {
		select {
		case call := <-n.calls:
			call(&session)
		case <-n.retire:
			session.retire(n.takeRetired())
		case <-n.quit:
			return
		}
	}
}

func (n *Notifier) toastEvent(kind int, shown *shownToast) {
	if kind == toastActivated && !n.clicked.fromToast(shown.tag) {
		n.activate(shown.tag)
	}
	n.retired.Lock()
	n.retired.toasts = append(n.retired.toasts, shown)
	n.retired.Unlock()
	select {
	case n.retire <- struct{}{}:
	default:
	}
}

func (n *Notifier) takeRetired() []*shownToast {
	n.retired.Lock()
	defer n.retired.Unlock()
	toasts := n.retired.toasts
	n.retired.toasts = nil
	return toasts
}

func setProcessAppID() error {
	if err := procSetCurrentProcessExplicitAppUserModelID.Find(); err != nil {
		return fmt.Errorf("set the Windows application identity %s: %w; run dalgona on Windows 10 or later", toastAppID, err)
	}
	id, err := windows.UTF16PtrFromString(toastAppID)
	if err != nil {
		return fmt.Errorf("set the Windows application identity %s: %w", toastAppID, err)
	}
	if hr, _, _ := procSetCurrentProcessExplicitAppUserModelID.Call(uintptr(unsafe.Pointer(id))); winrtFailed(hr) { //nolint:gosec // SetCurrentProcessExplicitAppUserModelID copies the NUL-terminated identity
		return winrtError("set the Windows application identity "+toastAppID+"; start dalgona before it opens a window", hr)
	}
	return nil
}

func (s *toastSession) open() error {
	if s.notifier != nil {
		return nil
	}
	if err := s.start(); err != nil {
		return err
	}
	statics, err := winrtFactory("Windows.UI.Notifications.ToastNotificationManager", &iidToastNotificationManagerStatics)
	if err != nil {
		return err
	}
	defer statics.release()
	appID, err := newWinrtString(toastAppID)
	if err != nil {
		return err
	}
	defer deleteWinrtString(appID)
	var notifier *winrtObject
	if hr := statics.call(toastManagerCreateNotifierWithID, unsafe.Pointer(appID), unsafe.Pointer(&notifier)); winrtFailed(hr) { //nolint:gosec // CreateToastNotifierWithId reads the identity HSTRING and writes the notifier
		return winrtError("create the Windows notifier for "+toastAppID+"; install dalgona with its Start menu shortcut", hr)
	}
	factory, err := winrtFactory("Windows.UI.Notifications.ToastNotification", &iidToastNotificationFactory)
	if err != nil {
		notifier.release()
		return err
	}
	s.notifier, s.factory = notifier, factory
	return nil
}

func (s *toastSession) start() error {
	if s.ready {
		return nil
	}
	for _, procedure := range winrtProcedures {
		if err := procedure.Find(); err != nil {
			return fmt.Errorf("load the Windows Runtime: %w; run dalgona on Windows 10 or later", err)
		}
	}
	if hr, _, _ := procRoInitialize.Call(winrtInitMultithreaded); winrtFailed(hr) {
		return winrtError("start the Windows Runtime on the notification thread; restart dalgona", hr)
	}
	if err := s.listen(); err != nil {
		_, _, _ = procRoUninitialize.Call()
		return err
	}
	s.ready = true
	return nil
}

func (s *toastSession) close() {
	for key, shown := range s.shown {
		shown.release()
		delete(s.shown, key)
	}
	s.notifier.release()
	s.factory.release()
	s.notifier, s.factory = nil, nil
	if s.ready {
		s.unlisten()
		_, _, _ = procRoUninitialize.Call()
		s.ready = false
	}
}

func (s *toastSession) permission() (NotificationPermission, error) {
	if err := s.open(); err != nil {
		return PermissionUnavailable, err
	}
	var setting toastSetting
	if hr := s.notifier.call(toastNotifierSetting, unsafe.Pointer(&setting)); winrtFailed(hr) { //nolint:gosec // get_Setting writes the NotificationSetting value
		return PermissionUnavailable, winrtError("read the Windows notification setting for "+toastAppID+"; install dalgona with its Start menu shortcut", hr)
	}
	return setting.permission(), nil
}

func (s *toastSession) show(n *Notifier, tag, content string) error {
	if err := s.open(); err != nil {
		return err
	}
	toast, err := s.createToast(content)
	if err != nil {
		return err
	}
	shown := &shownToast{owner: n, tag: tag, key: toastTag(tag), toast: toast}
	if err := shown.prepare(); err != nil {
		shown.release()
		return err
	}
	if hr := s.notifier.call(toastNotifierShow, unsafe.Pointer(toast)); winrtFailed(hr) { //nolint:gosec // Show takes the IToastNotification pointer
		shown.release()
		return winrtError("show the Windows notification; allow dalgona in Settings, Notifications", hr)
	}
	if previous := s.shown[shown.key]; previous != nil {
		previous.release()
	}
	s.shown[shown.key] = shown
	return nil
}

func (s *toastSession) createToast(content string) (*winrtObject, error) {
	document, err := loadToastDocument(content)
	if err != nil {
		return nil, err
	}
	defer document.release()
	var toast *winrtObject
	if hr := s.factory.call(toastFactoryCreate, unsafe.Pointer(document), unsafe.Pointer(&toast)); winrtFailed(hr) { //nolint:gosec // CreateToastNotification reads the IXmlDocument and writes the toast
		return nil, winrtError("create the Windows notification; send it again", hr)
	}
	return toast, nil
}

func (s *toastSession) retire(toasts []*shownToast) {
	for _, shown := range toasts {
		if s.shown[shown.key] == shown {
			delete(s.shown, shown.key)
		}
		shown.release()
	}
}

func loadToastDocument(content string) (*winrtObject, error) {
	instance, err := winrtInstance("Windows.Data.Xml.Dom.XmlDocument")
	if err != nil {
		return nil, err
	}
	defer instance.release()
	loader, err := instance.query(&iidXMLDocumentIO)
	if err != nil {
		return nil, err
	}
	defer loader.release()
	text, err := newWinrtString(content)
	if err != nil {
		return nil, err
	}
	defer deleteWinrtString(text)
	if hr := loader.call(xmlDocumentIOLoadXML, unsafe.Pointer(text)); winrtFailed(hr) { //nolint:gosec // LoadXml reads the content HSTRING
		return nil, winrtError("load the notification content; send it again", hr)
	}
	return instance.query(&iidXMLDocument)
}

func (t *shownToast) prepare() error {
	if err := t.label(); err != nil {
		return err
	}
	for kind, event := range toastEvents {
		handler := newToastHandler(kind, t)
		if hr := t.toast.call(event.add, unsafe.Pointer(handler), unsafe.Pointer(&t.tokens[kind])); winrtFailed(hr) { //nolint:gosec // the event source keeps the pinned handler and writes the registration token
			handler.release()
			return winrtError("watch the Windows notification for clicks; send it again", hr)
		}
		t.handlers[kind] = handler
	}
	return nil
}

func (t *shownToast) label() error {
	labels, err := t.toast.query(&iidToastNotification2)
	if err != nil {
		return err
	}
	defer labels.release()
	for _, property := range [...]struct {
		slot  int
		value string
	}{{toastNotification2PutTag, t.key}, {toastNotification2PutGroup, toastGroup}} {
		value, err := newWinrtString(property.value)
		if err != nil {
			return err
		}
		hr := labels.call(property.slot, unsafe.Pointer(value)) //nolint:gosec // put_Tag and put_Group read the HSTRING
		deleteWinrtString(value)
		if winrtFailed(hr) {
			return winrtError("tag the Windows notification; send it again", hr)
		}
	}
	return nil
}

func (t *shownToast) release() {
	if t.released {
		return
	}
	t.released = true
	for kind, handler := range t.handlers {
		if handler == nil {
			continue
		}
		_ = t.toast.callWord(toastEvents[kind].remove, t.tokens[kind])
		handler.release()
	}
	t.toast.release()
}

func newToastHandler(kind int, shown *shownToast) *toastHandler {
	toastHandlerVtable()
	handler := &toastHandler{vtbl: &toastHandlerSlots, iid: toastEvents[kind].iid, shown: shown, kind: kind}
	handler.refs.Store(1)
	handler.pinner.Pin(handler)
	return handler
}

func (h *toastHandler) release() uintptr {
	refs := h.refs.Add(^uint32(0))
	if refs == 0 {
		h.pinner.Unpin()
	}
	return uintptr(refs)
}

func toastHandlerQueryInterface(h *toastHandler, iid *winrtGUID, out *uintptr) uintptr {
	if out == nil || iid == nil {
		return uintptr(windows.E_POINTER)
	}
	if *iid != iidUnknown && *iid != iidAgileObject && *iid != *h.iid {
		*out = 0
		return uintptr(windows.E_NOINTERFACE)
	}
	h.refs.Add(1)
	*out = uintptr(unsafe.Pointer(h)) //nolint:gosec // the pinned handler stays valid until its last Release
	return uintptr(windows.S_OK)
}

func toastHandlerAddRef(h *toastHandler) uintptr {
	return uintptr(h.refs.Add(1))
}

func toastHandlerRelease(h *toastHandler) uintptr {
	return h.release()
}

func toastHandlerInvoke(h *toastHandler, _ uintptr, args *winrtObject) uintptr {
	if h.kind == toastDismissed && toastTimedOut(args) {
		return uintptr(windows.S_OK)
	}
	h.shown.owner.toastEvent(h.kind, h.shown)
	return uintptr(windows.S_OK)
}

func toastTimedOut(args *winrtObject) bool {
	var reason uint32
	if args == nil || winrtFailed(args.call(toastDismissedReason, unsafe.Pointer(&reason))) { //nolint:gosec // get_Reason writes the ToastDismissalReason value
		return false
	}
	return reason == toastDismissalTimedOut
}

func (o *winrtObject) call(slot int, args ...unsafe.Pointer) uintptr {
	var pinner runtime.Pinner
	defer pinner.Unpin()
	words := make([]uintptr, 1, 1+len(args))
	words[0] = uintptr(unsafe.Pointer(o)) //nolint:gosec // the interface pointer is the COM this argument
	for _, arg := range args {
		pinner.Pin(arg)
		words = append(words, uintptr(arg))
	}
	hr, _, _ := syscall.SyscallN(o.vtbl[slot], words...)
	return hr
}

func (o *winrtObject) callWord(slot int, word uintptr) uintptr {
	hr, _, _ := syscall.SyscallN(o.vtbl[slot], uintptr(unsafe.Pointer(o)), word) //nolint:gosec // the interface pointer is the COM this argument
	return hr
}

func (o *winrtObject) query(iid *winrtGUID) (*winrtObject, error) {
	var found *winrtObject
	if hr := o.call(winrtQueryInterface, unsafe.Pointer(iid), unsafe.Pointer(&found)); winrtFailed(hr) { //nolint:gosec // QueryInterface reads the IID and writes the interface pointer
		return nil, winrtError("query a Windows Runtime interface; update Windows", hr)
	}
	return found, nil
}

func (o *winrtObject) release() {
	if o != nil {
		_ = o.call(winrtRelease)
	}
}

func winrtFactory(class string, iid *winrtGUID) (*winrtObject, error) {
	name, err := newWinrtString(class)
	if err != nil {
		return nil, err
	}
	defer deleteWinrtString(name)
	var factory *winrtObject
	if hr, _, _ := procRoGetActivationFactory.Call(uintptr(unsafe.Pointer(name)), uintptr(unsafe.Pointer(iid)), uintptr(unsafe.Pointer(&factory))); winrtFailed(hr) { //nolint:gosec // RoGetActivationFactory reads the class name and IID and writes the factory
		return nil, winrtError("load the Windows Runtime class "+class+"; update Windows", hr)
	}
	return factory, nil
}

func winrtInstance(class string) (*winrtObject, error) {
	name, err := newWinrtString(class)
	if err != nil {
		return nil, err
	}
	defer deleteWinrtString(name)
	var instance *winrtObject
	if hr, _, _ := procRoActivateInstance.Call(uintptr(unsafe.Pointer(name)), uintptr(unsafe.Pointer(&instance))); winrtFailed(hr) { //nolint:gosec // RoActivateInstance reads the class name and writes the instance
		return nil, winrtError("create the Windows Runtime object "+class+"; update Windows", hr)
	}
	return instance, nil
}

func newWinrtString(text string) (*winrtString, error) {
	units := utf16.Encode([]rune(text))
	if len(units) == 0 {
		return nil, nil
	}
	var handle *winrtString
	if hr, _, _ := procWindowsCreateString.Call(uintptr(unsafe.Pointer(unsafe.SliceData(units))), uintptr(len(units)), uintptr(unsafe.Pointer(&handle))); winrtFailed(hr) { //nolint:gosec // WindowsCreateString copies len(units) UTF-16 units and writes the handle
		return nil, winrtError("copy text for the Windows Runtime; send it again", hr)
	}
	return handle, nil
}

func deleteWinrtString(handle *winrtString) {
	if handle != nil {
		_, _, _ = procWindowsDeleteString.Call(uintptr(unsafe.Pointer(handle))) //nolint:gosec // WindowsDeleteString frees the handle
	}
}

func winrtFailed(hr uintptr) bool {
	return hr&0x80000000 != 0
}

func winrtError(action string, hr uintptr) error {
	return fmt.Errorf("%s (HRESULT 0x%08X)", action, hr&winrtHResultBits)
}
