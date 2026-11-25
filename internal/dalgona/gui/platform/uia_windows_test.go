//go:build windows && (amd64 || arm64)

package platform

import (
	"errors"
	"image"
	"runtime"
	"strconv"
	"sync/atomic"
	"syscall"
	"testing"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	uiaTestUser32           = windows.NewLazySystemDLL("user32.dll")
	uiaTestRegisterClassEx  = uiaTestUser32.NewProc("RegisterClassExW")
	uiaTestCreateWindowEx   = uiaTestUser32.NewProc("CreateWindowExW")
	uiaTestDefWindowProc    = uiaTestUser32.NewProc("DefWindowProcW")
	uiaTestDestroyWindow    = uiaTestUser32.NewProc("DestroyWindow")
	uiaTestGetMessage       = uiaTestUser32.NewProc("GetMessageW")
	uiaTestTranslateMessage = uiaTestUser32.NewProc("TranslateMessage")
	uiaTestDispatchMessage  = uiaTestUser32.NewProc("DispatchMessageW")
	uiaTestPostQuitMessage  = uiaTestUser32.NewProc("PostQuitMessage")
	uiaTestOle32            = windows.NewLazySystemDLL("ole32.dll")
	uiaTestCoCreateInstance = uiaTestOle32.NewProc("CoCreateInstance")
	uiaTestSafeArrayGetElem = uiaOleaut32.NewProc("SafeArrayGetElement")
	uiaTestSafeArrayUBound  = uiaOleaut32.NewProc("SafeArrayGetUBound")
	uiaTestCLSIDAutomation  = windows.GUID{Data1: 0xff48dba4, Data2: 0x60ef, Data3: 0x4201, Data4: [8]byte{0xaa, 0x87, 0x54, 0x10, 0x3e, 0xef, 0x59, 0x4e}}
	uiaTestIIDAutomation    = windows.GUID{Data1: 0x30cbe57d, Data2: 0xd9d0, Data3: 0x452a, Data4: [8]byte{0xab, 0x13, 0x7a, 0xc5, 0xac, 0x48, 0x25, 0xee}}
	uiaTestIIDInvokePattern = windows.GUID{Data1: 0xfb377fbe, Data2: 0x8ea6, Data3: 0x46d5, Data4: [8]byte{0x9c, 0x73, 0x64, 0x99, 0x64, 0x2d, 0x30, 0x59}}
	uiaTestWindowClasses    atomic.Int32
)

type uiaTestCOM struct {
	vtbl *[128]uintptr
}

type uiaTestVariant struct {
	kind     uint16
	reserved [3]uint16
	text     *uint16
	record   uintptr
}

type uiaTestMessage struct {
	hwnd    uintptr
	message uint32
	wParam  uintptr
	lParam  uintptr
	time    uint32
	x, y    int32
	private uint32
}

type uiaTestWindowClass struct {
	size       uint32
	style      uint32
	procedure  uintptr
	classExtra int32
	extra      int32
	instance   uintptr
	icon       uintptr
	cursor     uintptr
	background uintptr
	menu       *uint16
	name       *uint16
	smallIcon  uintptr
}

type uiaTestWindow struct {
	hwnd  uintptr
	calls chan func()
	ended chan struct{}
}

func uiaOpenTestWindow(t *testing.T) *uiaTestWindow {
	t.Helper()
	w := &uiaTestWindow{calls: make(chan func(), 1), ended: make(chan struct{})}
	ready := make(chan error, 1)
	go w.loop(ready)
	if err := <-ready; err != nil {
		t.Fatal(err)
	}
	t.Cleanup(w.close)
	return w
}

func (w *uiaTestWindow) loop(ready chan<- error) {
	runtime.LockOSThread()
	defer close(w.ended)
	name, _ := windows.UTF16PtrFromString("DalgonaUIATest" + strconv.Itoa(int(uiaTestWindowClasses.Add(1))))
	var instance windows.Handle
	if err := windows.GetModuleHandleEx(0, nil, &instance); err != nil {
		ready <- err
		return
	}
	class := uiaTestWindowClass{procedure: uiaTestDefWindowProc.Addr(), instance: uintptr(instance), name: name}
	class.size = uint32(unsafe.Sizeof(class))
	if atom, _, err := uiaTestRegisterClassEx.Call(uintptr(unsafe.Pointer(&class))); atom == 0 { //nolint:gosec // Win32 reads the sized window class during registration
		ready <- err
		return
	}
	hwnd, _, err := uiaTestCreateWindowEx.Call(0x08|0x80|0x08000000, uintptr(unsafe.Pointer(name)), uintptr(unsafe.Pointer(name)), 0x80000000|0x10000000, 100, 100, 400, 300, 0, 0, uintptr(instance), 0) //nolint:gosec // Win32 reads these live, NUL-terminated UTF-16 strings
	if hwnd == 0 {
		ready <- err
		return
	}
	w.hwnd = hwnd
	ready <- nil
	var msg uiaTestMessage
	for {
		if got, _, _ := uiaTestGetMessage.Call(uintptr(unsafe.Pointer(&msg)), 0, 0, 0); got&0xFFFFFFFF == 0 || got&0xFFFFFFFF == 0xFFFFFFFF { //nolint:gosec // GetMessageW fills one MSG
			return
		}
		_, _, _ = uiaTestTranslateMessage.Call(uintptr(unsafe.Pointer(&msg))) //nolint:gosec // TranslateMessage reads the MSG filled by GetMessageW
		_, _, _ = uiaTestDispatchMessage.Call(uintptr(unsafe.Pointer(&msg)))  //nolint:gosec // DispatchMessageW reads the MSG filled by GetMessageW
		w.drain()
	}
}

func (w *uiaTestWindow) drain() {
	for {
		select {
		case f := <-w.calls:
			f()
		default:
			return
		}
	}
}

func (w *uiaTestWindow) run(f func()) {
	done := make(chan struct{})
	w.calls <- func() {
		defer close(done)
		f()
	}
	_, _, _ = uiaProcPostMessage.Call(w.hwnd, uiaWMNull, 0, 0)
	<-done
}

func (w *uiaTestWindow) close() {
	w.run(func() {
		_, _, _ = uiaTestDestroyWindow.Call(w.hwnd)
		_, _, _ = uiaTestPostQuitMessage.Call(0)
	})
	<-w.ended
}

func uiaTestTree(t *testing.T, revision uint64, withSend bool) *SemanticSnapshot {
	t.Helper()
	nodes := []SemanticNode{
		{ID: "doc", Role: RoleDocument, Name: "Transcript", Bounds: image.Rect(0, 0, 400, 300)},
		{ID: "name", Parent: "doc", Role: RoleTextField, Name: "Name", Action: "Edit", Value: "Ada", Bounds: image.Rect(20, 100, 300, 140)},
	}
	if withSend {
		nodes = append(nodes, SemanticNode{ID: "send", Parent: "doc", Role: RoleButton, Name: "Send", Action: "Send", Bounds: image.Rect(20, 20, 120, 60)})
	}
	return uiaTestSnapshot(t, revision, "name", nodes...)
}

func uiaComRelease(object *uiaTestCOM) {
	if object != nil {
		_, _, _ = syscall.SyscallN(object.vtbl[2], uintptr(unsafe.Pointer(object))) //nolint:gosec // IUnknown::Release takes the live COM receiver
	}
}

func uiaClientAutomation(t *testing.T) *uiaTestCOM {
	t.Helper()
	runtime.LockOSThread()
	t.Cleanup(runtime.UnlockOSThread)
	if err := windows.CoInitializeEx(0, windows.COINIT_MULTITHREADED); err != nil && !errors.Is(err, windows.Errno(windows.S_FALSE)) {
		t.Fatal(err)
	}
	t.Cleanup(windows.CoUninitialize)
	var automation *uiaTestCOM
	hr, _, _ := uiaTestCoCreateInstance.Call(uintptr(unsafe.Pointer(&uiaTestCLSIDAutomation)), 0, 1, uintptr(unsafe.Pointer(&uiaTestIIDAutomation)), uintptr(unsafe.Pointer(&automation))) //nolint:gosec // CoCreateInstance reads two GUIDs and writes the interface pointer
	if uiaFailed(hr) || automation == nil {
		t.Fatalf("CoCreateInstance(CUIAutomation) failed with HRESULT %#x", hr)
	}
	t.Cleanup(func() { uiaComRelease(automation) })
	return automation
}

func uiaClientString(t *testing.T, element *uiaTestCOM, property int32) string {
	t.Helper()
	var v uiaTestVariant
	hr, _, _ := syscall.SyscallN(element.vtbl[10], uintptr(unsafe.Pointer(element)), uintptr(property), uintptr(unsafe.Pointer(&v))) //nolint:gosec // GetCurrentPropertyValue writes one ABI-sized VARIANT
	if uiaFailed(hr) {
		t.Fatalf("GetCurrentPropertyValue(%d) failed with HRESULT %#x", property, hr)
	}
	defer (*uiaVariant)(unsafe.Pointer(&v)).clear() //nolint:gosec // both structs have the Windows VARIANT ABI
	if v.kind != uiaVTBSTR {
		t.Fatalf("property %d has VARTYPE %d, want VT_BSTR", property, v.kind)
	}
	return windows.UTF16PtrToString(v.text)
}

func uiaClientElementAt(t *testing.T, automation *uiaTestCOM, hwnd uintptr, client image.Point) *uiaTestCOM {
	t.Helper()
	origin, ok := uiaClientOrigin(hwnd)
	if !ok {
		t.Fatal("ClientToScreen failed for the test window")
	}
	x, y := int64(client.X)+int64(origin.x), int64(client.Y)+int64(origin.y)
	point := uintptr(x&0xffffffff) | uintptr(y&0xffffffff)<<32
	var element *uiaTestCOM
	hr, _, _ := syscall.SyscallN(automation.vtbl[7], uintptr(unsafe.Pointer(automation)), point, uintptr(unsafe.Pointer(&element))) //nolint:gosec // ElementFromPoint writes one COM interface pointer
	if uiaFailed(hr) {
		t.Fatalf("ElementFromPoint failed with HRESULT %#x", hr)
	}
	t.Cleanup(func() { uiaComRelease(element) })
	return element
}

func TestUIAAttachRefusesCallsOffTheWindowThread(t *testing.T) {
	w := uiaOpenTestWindow(t)
	tree := NewAccessibilityTree(4, nil)
	if detach, err := AttachUIA(w.hwnd, tree); err == nil {
		detach()
		t.Fatal("AttachUIA subclassed a window from a foreign thread")
	}
	if _, err := AttachUIA(0, tree); err == nil {
		t.Fatal("AttachUIA accepted a null window")
	}
}

func TestUIAProvidersAnswerThroughTheirVtables(t *testing.T) {
	if err := uiaReady(); err != nil {
		t.Fatal(err)
	}
	tree := NewAccessibilityTree(4, nil)
	tree.Publish(uiaTestTree(t, 1, true))
	b := &uiaBridge{elements: map[NodeID]*uiaElement{}}
	b.tree.Store(tree)
	b.window = newUIAElement(b, "", 0)
	send := b.element("send", RoleButton)
	checkUIAIdentity(t, send)
	checkUIAProperties(t, send)
	if hr, _, _ := syscall.SyscallN(uiaCallbacks.invoke[3], uiaPointer(&send.invoke)); hr != uiaOK {
		t.Fatalf("Invoke = %#x", hr)
	}
	checkUIAInvoke(t, tree)
	tree.Publish(uiaTestTree(t, 2, false))
	b.publish(uiaTestTree(t, 1, true))
	var name uiaVariant
	if hr, _, _ := syscall.SyscallN(uiaCallbacks.simple[5], uiaPointer(&send.simple), 30005, uintptr(unsafe.Pointer(&name))); hr != uiaUnavailable { //nolint:gosec // GetPropertyValue takes a VARIANT destination
		t.Fatalf("a removed element answered GetPropertyValue with %#x, want UIA_E_ELEMENTNOTAVAILABLE", hr)
	}
	if again := b.element("send", RoleButton); again == send {
		t.Fatal("a retired element was reused for a returning node identity")
	}
}

func checkUIAIdentity(t *testing.T, send *uiaElement) {
	t.Helper()
	var out uintptr
	if hr, _, _ := syscall.SyscallN(uiaCallbacks.simple[0], uiaPointer(&send.simple), uintptr(unsafe.Pointer(&uiaIIDFragment)), uintptr(unsafe.Pointer(&out))); hr != uiaOK || out != uiaPointer(&send.fragment) { //nolint:gosec // QueryInterface reads a GUID and writes one interface pointer
		t.Fatalf("QueryInterface(IRawElementProviderFragment) = %#x, %#x", hr, out)
	}
	if hr, _, _ := syscall.SyscallN(uiaCallbacks.fragment[0], uiaPointer(&send.fragment), uintptr(unsafe.Pointer(&uiaIIDUnknown)), uintptr(unsafe.Pointer(&out))); hr != uiaOK || out != uiaPointer(&send.simple) { //nolint:gosec // QueryInterface reads a GUID and writes one interface pointer
		t.Fatalf("QueryInterface(IUnknown) through the fragment = %#x, %#x; COM identity needs the simple pointer", hr, out)
	}
	if hr, _, _ := syscall.SyscallN(uiaCallbacks.simple[0], uiaPointer(&send.simple), uintptr(unsafe.Pointer(&uiaIIDValue)), uintptr(unsafe.Pointer(&out))); [2]uintptr{hr, out} != [2]uintptr{uiaNoInterface, 0} { //nolint:gosec // QueryInterface reads a GUID and writes one interface pointer
		t.Fatalf("a button answered QueryInterface(IValueProvider) with %#x, %#x", hr, out)
	}
	if refs, _, _ := syscall.SyscallN(uiaCallbacks.simple[2], uiaPointer(&send.simple)); refs != 1 {
		t.Fatalf("Release left %d references, want 1", refs)
	}
	if refs, _, _ := syscall.SyscallN(uiaCallbacks.simple[2], uiaPointer(&send.simple)); refs != 0 || uiaLookup(uiaPointer(&send.simple)) != nil {
		t.Fatalf("the last Release left %d references or a live registration", refs)
	}
}

func checkUIAProperties(t *testing.T, send *uiaElement) {
	t.Helper()
	var name uiaVariant
	if hr, _, _ := syscall.SyscallN(uiaCallbacks.simple[5], uiaPointer(&send.simple), 30005, uintptr(unsafe.Pointer(&name))); hr != uiaOK || name.kind != uiaVTBSTR { //nolint:gosec // GetPropertyValue takes a VARIANT destination
		t.Fatalf("GetPropertyValue(UIA_NamePropertyId) = %#x with VARTYPE %d", hr, name.kind)
	}
	name.clear()
	var ids uintptr
	if hr, _, _ := syscall.SyscallN(uiaCallbacks.fragment[4], uiaPointer(&send.fragment), uintptr(unsafe.Pointer(&ids))); hr != uiaOK || ids == 0 { //nolint:gosec // GetRuntimeId writes one SAFEARRAY pointer
		t.Fatalf("GetRuntimeId = %#x", hr)
	}
	defer func() { _, _, _ = uiaProcSafeArrayDestroy.Call(ids) }()
	want := uiaRuntimeID("send")
	var upper int32
	_, _, _ = uiaTestSafeArrayUBound.Call(ids, 1, uintptr(unsafe.Pointer(&upper))) //nolint:gosec // SafeArrayGetUBound writes one LONG
	if upper != 2 {
		t.Fatalf("runtime id array upper bound %d, want 2", upper)
	}
	for i, id := range want {
		index, got := int32(i), int32(0)
		_, _, _ = uiaTestSafeArrayGetElem.Call(ids, uintptr(unsafe.Pointer(&index)), uintptr(unsafe.Pointer(&got))) //nolint:gosec // one array index and one LONG result
		if got != id {
			t.Fatalf("runtime id element %d of %d = %d, want %v", i, upper+1, got, want)
		}
	}
}

func checkUIAInvoke(t *testing.T, tree *AccessibilityTree) {
	t.Helper()
	select {
	case intent := <-tree.Intents():
		if intent.Node != "send" || intent.Action != ActionActivate || intent.Revision != 1 {
			t.Fatalf("Invoke published %+v", intent)
		}
	default:
		t.Fatal("Invoke published no intent")
	}
}

func TestUIAClientsReadAndActThroughTheAttachedWindow(t *testing.T) {
	w := uiaOpenTestWindow(t)
	tree := NewAccessibilityTree(4, nil)
	tree.Publish(uiaTestTree(t, 1, true))
	var detach func()
	var attachErr error
	w.run(func() { detach, attachErr = AttachUIA(w.hwnd, tree) })
	if attachErr != nil {
		t.Fatal(attachErr)
	}
	t.Cleanup(detach)
	automation := uiaClientAutomation(t)
	send := uiaClientElementAt(t, automation, w.hwnd, image.Pt(70, 40))
	if got := uiaClientString(t, send, 30011); got != "send" {
		t.Fatalf("ElementFromPoint over the button found %q", got)
	}
	field := uiaClientElementAt(t, automation, w.hwnd, image.Pt(60, 120))
	if got := uiaClientString(t, field, 30045); got != "Ada" {
		t.Fatalf("the text field value read %q", got)
	}
	checkUIAClientInvoke(t, send, tree)
	tree.Publish(uiaTestTree(t, 2, false))
	waitUIARemoval(t, send)
	detach()
	waitUIADetach(t)
}

func checkUIAClientInvoke(t *testing.T, send *uiaTestCOM, tree *AccessibilityTree) {
	t.Helper()
	var invoke *uiaTestCOM
	hr, _, _ := syscall.SyscallN(send.vtbl[14], uintptr(unsafe.Pointer(send)), 10000, uintptr(unsafe.Pointer(&uiaTestIIDInvokePattern)), uintptr(unsafe.Pointer(&invoke))) //nolint:gosec // GetCurrentPatternAs reads a GUID and writes one interface pointer
	if uiaFailed(hr) || invoke == nil {
		t.Fatalf("GetCurrentPatternAs failed with HRESULT %#x", hr)
	}
	defer uiaComRelease(invoke)
	if hr, _, _ = syscall.SyscallN(invoke.vtbl[3], uintptr(unsafe.Pointer(invoke))); uiaFailed(hr) { //nolint:gosec // Invoke takes the live COM receiver
		t.Fatalf("Invoke failed with HRESULT %#x", hr)
	}
	select {
	case intent := <-tree.Intents():
		if intent.Node != "send" || intent.Action != ActionActivate {
			t.Fatalf("Invoke published %+v", intent)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Invoke published no intent")
	}
}

func waitUIARemoval(t *testing.T, send *uiaTestCOM) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	var v uiaTestVariant
	for {
		hr, _, _ := syscall.SyscallN(send.vtbl[10], uintptr(unsafe.Pointer(send)), 30005, uintptr(unsafe.Pointer(&v))) //nolint:gosec // GetCurrentPropertyValue writes one ABI-sized VARIANT
		if hr == uiaUnavailable {
			break
		}
		(*uiaVariant)(unsafe.Pointer(&v)).clear() //nolint:gosec // both structs have the Windows VARIANT ABI
		if time.Now().After(deadline) {
			t.Fatalf("a removed element still answered with %#x", hr)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func waitUIADetach(t *testing.T) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		active := 0
		uiaBridges.Range(func(any, any) bool { active++; return true })
		if active == 0 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("detach left the window subclass installed")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestUIATextRangeRemoveFromSelectionIsUnsupported(t *testing.T) {
	if got := uiaRangeRemoveFromSelection(&uiaInterface{}); got != uiaInvalidOperation {
		t.Fatalf("RemoveFromSelection returned %#x, want the unsupported-operation result", got)
	}
}
