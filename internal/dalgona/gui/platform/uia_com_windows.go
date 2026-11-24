//go:build windows && (amd64 || arm64)

package platform

import (
	"errors"
	"fmt"
	"runtime"
	"sync"
	"syscall"
	"unicode/utf16"
	"unsafe"

	"github.com/gosuda/dalgona/internal/dal/inputtext"

	"golang.org/x/sys/windows"
)

const (
	uiaOK               uintptr = 0
	uiaInvalidArg       uintptr = 0x80070057
	uiaNoInterface      uintptr = 0x80004002
	uiaNullPointer      uintptr = 0x80004003
	uiaFail             uintptr = 0x80004005
	uiaAccessDenied     uintptr = 0x80070005
	uiaOutOfMemory      uintptr = 0x8007000E
	uiaRetryLater       uintptr = 0x8001010A
	uiaNotEnabled       uintptr = 0x80040200
	uiaUnavailable      uintptr = 0x80040201
	uiaInvalidOperation uintptr = 0x80131509
)

const (
	uiaVTI4      uint16 = 3
	uiaVTR8      uint16 = 5
	uiaVTBSTR    uint16 = 8
	uiaVTBool    uint16 = 11
	uiaVTUnknown uint16 = 13
	uiaVTArray   uint16 = 0x2000
)

var (
	uiaIIDUnknown        = windows.GUID{Data1: 0x00000000, Data2: 0x0000, Data3: 0x0000, Data4: [8]byte{0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46}}
	uiaIIDSimple         = windows.GUID{Data1: 0xd6dd68d1, Data2: 0x86fd, Data3: 0x4332, Data4: [8]byte{0x86, 0x66, 0x9a, 0xbe, 0xde, 0xa2, 0xd2, 0x4c}}
	uiaIIDFragment       = windows.GUID{Data1: 0xf7063da8, Data2: 0x8359, Data3: 0x439c, Data4: [8]byte{0x92, 0x97, 0xbb, 0xc5, 0x29, 0x9a, 0x7d, 0x87}}
	uiaIIDFragmentRoot   = windows.GUID{Data1: 0x620ce2a5, Data2: 0xab8f, Data3: 0x40a9, Data4: [8]byte{0x86, 0xcb, 0xde, 0x3c, 0x75, 0x59, 0x9b, 0x58}}
	uiaIIDInvoke         = windows.GUID{Data1: 0x54fcb24b, Data2: 0xe18e, Data3: 0x47a2, Data4: [8]byte{0xb4, 0xd3, 0xec, 0xcb, 0xe7, 0x75, 0x99, 0xa2}}
	uiaIIDValue          = windows.GUID{Data1: 0xc7935180, Data2: 0x6fb3, Data3: 0x4201, Data4: [8]byte{0xb1, 0x74, 0x7d, 0xf7, 0x3a, 0xdb, 0xf6, 0x4a}}
	uiaIIDToggle         = windows.GUID{Data1: 0x56d00bd0, Data2: 0xc4f4, Data3: 0x433c, Data4: [8]byte{0xa8, 0x36, 0x1a, 0x52, 0xa5, 0x7e, 0x08, 0x92}}
	uiaIIDSelectionItem  = windows.GUID{Data1: 0x2acad808, Data2: 0xb2d4, Data3: 0x452d, Data4: [8]byte{0xa4, 0x07, 0x91, 0xff, 0x1a, 0xd1, 0x67, 0xb2}}
	uiaIIDExpandCollapse = windows.GUID{Data1: 0xd847d3a5, Data2: 0xcab0, Data3: 0x4a98, Data4: [8]byte{0x8c, 0x32, 0xec, 0xb4, 0x5c, 0x59, 0xad, 0x24}}
	uiaIIDScrollItem     = windows.GUID{Data1: 0x2360c714, Data2: 0x4bf1, Data3: 0x4b26, Data4: [8]byte{0xba, 0x65, 0x9b, 0x21, 0x31, 0x61, 0x27, 0xeb}}
	uiaIIDText           = windows.GUID{Data1: 0x3589c92c, Data2: 0x63f3, Data3: 0x4367, Data4: [8]byte{0x99, 0xbb, 0xad, 0xa6, 0x53, 0xb7, 0x7c, 0xf2}}
	uiaIIDTextRange      = windows.GUID{Data1: 0x5347ad7b, Data2: 0xc355, Data3: 0x46f8, Data4: [8]byte{0xaf, 0xf5, 0x90, 0x90, 0x33, 0x58, 0x2f, 0x63}}
	uiaIIDGrid           = windows.GUID{Data1: 0xb17d6187, Data2: 0x0907, Data3: 0x464b, Data4: [8]byte{0xa1, 0x68, 0x0e, 0xf1, 0x7a, 0x15, 0x72, 0xb1}}
	uiaIIDGridItem       = windows.GUID{Data1: 0xd02541f1, Data2: 0xfb81, Data3: 0x4d64, Data4: [8]byte{0xae, 0x32, 0xf5, 0x20, 0xf8, 0xa6, 0xdb, 0xd1}}
	uiaIIDTable          = windows.GUID{Data1: 0x9c860395, Data2: 0x97b3, Data3: 0x490a, Data4: [8]byte{0xb5, 0x2a, 0x85, 0x8c, 0xc2, 0x2a, 0xf1, 0x66}}
	uiaIIDTableItem      = windows.GUID{Data1: 0xb9734fa6, Data2: 0x771f, Data3: 0x4d78, Data4: [8]byte{0x9c, 0x90, 0x25, 0x17, 0x99, 0x93, 0x49, 0xcd}}
)

var (
	uiaCore                             = windows.NewLazySystemDLL("uiautomationcore.dll")
	procUiaReturnRawElementProvider     = uiaCore.NewProc("UiaReturnRawElementProvider")
	procUiaHostProviderFromHwnd         = uiaCore.NewProc("UiaHostProviderFromHwnd")
	procUiaRaiseAutomationEvent         = uiaCore.NewProc("UiaRaiseAutomationEvent")
	procUiaRaisePropertyChangedEvent    = uiaCore.NewProc("UiaRaiseAutomationPropertyChangedEvent")
	procUiaRaiseStructureChangedEvent   = uiaCore.NewProc("UiaRaiseStructureChangedEvent")
	procUiaRaiseNotificationEvent       = uiaCore.NewProc("UiaRaiseNotificationEvent")
	procUiaClientsAreListening          = uiaCore.NewProc("UiaClientsAreListening")
	procUiaDisconnectProvider           = uiaCore.NewProc("UiaDisconnectProvider")
	procUiaGetReservedNotSupportedValue = uiaCore.NewProc("UiaGetReservedNotSupportedValue")
	uiaComctl32                         = windows.NewLazySystemDLL("comctl32.dll")
	uiaProcSetWindowSubclass            = uiaComctl32.NewProc("SetWindowSubclass")
	uiaProcRemoveWindowSubclass         = uiaComctl32.NewProc("RemoveWindowSubclass")
	uiaProcDefSubclassProc              = uiaComctl32.NewProc("DefSubclassProc")
	uiaUser32                           = windows.NewLazySystemDLL("user32.dll")
	uiaProcClientToScreen               = uiaUser32.NewProc("ClientToScreen")
	uiaProcPostMessage                  = uiaUser32.NewProc("PostMessageW")
	uiaOleaut32                         = windows.NewLazySystemDLL("oleaut32.dll")
	uiaProcSysAllocStringLen            = uiaOleaut32.NewProc("SysAllocStringLen")
	uiaProcSysStringLen                 = uiaOleaut32.NewProc("SysStringLen")
	uiaProcSysFreeString                = uiaOleaut32.NewProc("SysFreeString")
	uiaProcSafeArrayCreateVector        = uiaOleaut32.NewProc("SafeArrayCreateVector")
	uiaProcSafeArrayPutElement          = uiaOleaut32.NewProc("SafeArrayPutElement")
	uiaProcSafeArrayDestroy             = uiaOleaut32.NewProc("SafeArrayDestroy")
	uiaProcVariantClear                 = uiaOleaut32.NewProc("VariantClear")
	uiaProcedures                       = []*windows.LazyProc{procUiaReturnRawElementProvider, procUiaHostProviderFromHwnd, procUiaRaiseAutomationEvent, procUiaRaisePropertyChangedEvent, procUiaRaiseStructureChangedEvent, procUiaRaiseNotificationEvent, procUiaClientsAreListening, procUiaDisconnectProvider, procUiaGetReservedNotSupportedValue, uiaProcSetWindowSubclass, uiaProcRemoveWindowSubclass, uiaProcDefSubclassProc, uiaProcClientToScreen, uiaProcPostMessage, uiaProcSysAllocStringLen, uiaProcSysStringLen, uiaProcSysFreeString, uiaProcSafeArrayCreateVector, uiaProcSafeArrayPutElement, uiaProcSafeArrayDestroy, uiaProcVariantClear}
	errUIAUnavailable                   = errors.New("attach UI Automation: a required system function is missing; run on Windows 10 version 1709 or later")
)

type uiaInterface struct {
	vtbl  *uintptr
	owner uiaObject
}

type uiaObject interface {
	identity() *uiaInterface
	references() *uiaReferences
	query(iid *windows.GUID) *uiaInterface
}

type uiaReferences struct {
	count  uint32
	pinner runtime.Pinner
}

var uiaLive = struct {
	sync.Mutex
	objects map[uintptr]uiaObject
}{objects: make(map[uintptr]uiaObject)}

func uiaPointer(p *uiaInterface) uintptr {
	return uintptr(unsafe.Pointer(p)) //nolint:gosec // COM receives the interface address and the object stays pinned while referenced
}

func uiaAddRef(o uiaObject) uint32 {
	uiaLive.Lock()
	defer uiaLive.Unlock()
	refs := o.references()
	refs.count++
	if refs.count == 1 {
		refs.pinner.Pin(o.identity())
		uiaLive.objects[uiaPointer(o.identity())] = o
	}
	return refs.count
}

func uiaRelease(o uiaObject) uint32 {
	uiaLive.Lock()
	defer uiaLive.Unlock()
	refs := o.references()
	if refs.count == 0 {
		return 0
	}
	refs.count--
	if refs.count == 0 {
		refs.pinner.Unpin()
		delete(uiaLive.objects, uiaPointer(o.identity()))
	}
	return refs.count
}

func uiaLookup(p uintptr) uiaObject {
	uiaLive.Lock()
	defer uiaLive.Unlock()
	return uiaLive.objects[p]
}

func uiaHandOut(out *uintptr, o uiaObject, iface *uiaInterface) uintptr {
	uiaAddRef(o)
	*out = uiaPointer(iface)
	return uiaOK
}

func uiaQueryInterface(this *uiaInterface, iid *windows.GUID, out *uintptr) uintptr {
	if out == nil {
		return uiaNullPointer
	}
	*out = 0
	if iid == nil {
		return uiaInvalidArg
	}
	found := this.owner.query(iid)
	if found == nil {
		return uiaNoInterface
	}
	return uiaHandOut(out, this.owner, found)
}

func uiaAddRefMethod(this *uiaInterface) uintptr {
	return uintptr(uiaAddRef(this.owner))
}

func uiaReleaseMethod(this *uiaInterface) uintptr {
	return uintptr(uiaRelease(this.owner))
}

func uiaFailed(hr uintptr) bool {
	return hr&0x80000000 != 0
}

type uiaVariant struct {
	kind     uint16
	reserved [3]uint16
	value    uintptr
	record   uintptr
}

func (v *uiaVariant) setInt(n int32) {
	v.kind = uiaVTI4
	*(*int32)(unsafe.Pointer(&v.value)) = n //nolint:gosec // VARIANT stores lVal in the first union bytes
}

func (v *uiaVariant) setBool(b bool) {
	v.kind = uiaVTBool
	if b {
		*(*int16)(unsafe.Pointer(&v.value)) = -1 //nolint:gosec // VARIANT stores VARIANT_TRUE as boolVal in the first union bytes
	}
}

func (v *uiaVariant) setString(s string) uintptr {
	text := uiaBSTR(s)
	if text == 0 {
		return uiaOutOfMemory
	}
	v.kind, v.value = uiaVTBSTR, text
	return uiaOK
}

func (v *uiaVariant) set(value uiaValue) uintptr {
	switch value.kind {
	case uiaEmpty, uiaElements:
	case uiaString:
		return v.setString(value.text)
	case uiaBool:
		v.setBool(value.flag)
	case uiaInt:
		v.setInt(value.number)
	}
	return uiaOK
}

func (v *uiaVariant) clear() {
	_, _, _ = uiaProcVariantClear.Call(uintptr(unsafe.Pointer(v))) //nolint:gosec // VariantClear frees the BSTR or SAFEARRAY the variant owns
}

func (v *uiaVariant) flag() bool {
	return v.kind == uiaVTBool && *(*int16)(unsafe.Pointer(&v.value)) != 0 //nolint:gosec // VARIANT stores boolVal in the first union bytes
}

func uiaBSTR(s string) uintptr {
	units := make([]uint16, 0, len(s))
	for _, r := range s {
		units = utf16.AppendRune(units, r)
	}
	text, _, _ := uiaProcSysAllocStringLen.Call(uintptr(unsafe.Pointer(unsafe.SliceData(units))), uintptr(len(units))) //nolint:gosec // SysAllocStringLen copies the UTF-16 units
	return text
}

func uiaFromBSTR(text *uint16) (string, bool) {
	if text == nil {
		return "", true
	}
	n, _, _ := uiaProcSysStringLen.Call(uintptr(unsafe.Pointer(text))) //nolint:gosec // SysStringLen reads the BSTR length prefix
	if n > transferLimit/2 {
		return "", false
	}
	return uiaDecodeUnits(unsafe.Slice(text, n)) //nolint:gosec // the BSTR holds n UTF-16 units
}

func uiaFromWide(text *uint16) (string, bool) {
	if text == nil {
		return "", true
	}
	base := unsafe.Pointer(text) //nolint:gosec // LPCWSTR points at NUL-terminated UTF-16 units
	for n := range transferLimit / 2 {
		if *(*uint16)(unsafe.Add(base, 2*n)) == 0 { //nolint:gosec // the scan stops at the terminating NUL
			return uiaDecodeUnits(unsafe.Slice(text, n)) //nolint:gosec // the string holds n UTF-16 units before NUL
		}
	}
	return "", false
}

func uiaDecodeUnits(units []uint16) (string, bool) {
	s, err := inputtext.DecodeUTF16(units)
	return s, err == nil
}

func uiaArray(kind uint16, n int, element func(i int) unsafe.Pointer, out *uintptr) uintptr {
	array, _, _ := uiaProcSafeArrayCreateVector.Call(uintptr(kind), 0, uintptr(n))
	if array == 0 {
		return uiaOutOfMemory
	}
	for i := int32(0); int(i) < n; i++ {
		hr, _, _ := uiaProcSafeArrayPutElement.Call(array, uintptr(unsafe.Pointer(&i)), uintptr(element(int(i)))) //nolint:gosec // SafeArrayPutElement copies the value or AddRefs the interface
		if uiaFailed(hr) {
			_, _, _ = uiaProcSafeArrayDestroy.Call(array)
			return hr
		}
	}
	*out = array
	return uiaOK
}

func uiaIntArray(values []int32, out *uintptr) uintptr {
	return uiaArray(uiaVTI4, len(values), func(i int) unsafe.Pointer { return unsafe.Pointer(&values[i]) }, out) //nolint:gosec // SafeArrayPutElement reads one LONG
}

func uiaDoubleArray(values []float64, out *uintptr) uintptr {
	return uiaArray(uiaVTR8, len(values), func(i int) unsafe.Pointer { return unsafe.Pointer(&values[i]) }, out) //nolint:gosec // SafeArrayPutElement reads one double
}

func uiaObjectArray(objects []*uiaInterface, out *uintptr) uintptr {
	return uiaArray(uiaVTUnknown, len(objects), func(i int) unsafe.Pointer { return unsafe.Pointer(objects[i]) }, out) //nolint:gosec // SafeArrayPutElement AddRefs the interface pointer
}

type uiaWindowPoint struct {
	x, y int32
}

type uiaRect struct {
	left, top, width, height float64
}

func uiaClientOrigin(hwnd uintptr) (uiaWindowPoint, bool) {
	var origin uiaWindowPoint
	ok, _, _ := uiaProcClientToScreen.Call(hwnd, uintptr(unsafe.Pointer(&origin))) //nolint:gosec // ClientToScreen writes the converted POINT
	return origin, ok != 0
}

func uiaLoadProcedures() error {
	for _, p := range uiaProcedures {
		if err := p.Find(); err != nil {
			return fmt.Errorf("%w: %w", errUIAUnavailable, err)
		}
	}
	return nil
}

var uiaReady = sync.OnceValue(func() error {
	if err := uiaLoadProcedures(); err != nil {
		return err
	}
	uiaBuildVtables()
	return nil
})

var uiaCallbacks struct {
	unknown   [3]uintptr
	simple    [7]uintptr
	fragment  [9]uintptr
	root      [5]uintptr
	invoke    [4]uintptr
	value     [6]uintptr
	toggle    [5]uintptr
	selection [8]uintptr
	expand    [6]uintptr
	scroll    [4]uintptr
	text      [9]uintptr
	textRange [21]uintptr
	grid      [6]uintptr
	gridItem  [8]uintptr
	table     [6]uintptr
	tableItem [5]uintptr
	subclass  uintptr
}

func uiaBuildVtables() {
	v := &uiaCallbacks
	v.unknown = [3]uintptr{syscall.NewCallback(uiaQueryInterface), syscall.NewCallback(uiaAddRefMethod), syscall.NewCallback(uiaReleaseMethod)}
	uiaElementFromPointCallback = syscall.NewCallback(uiaElementFromPoint)
	uiaRangeFromPointCallback = syscall.NewCallback(uiaRangeFromPoint)
	elementFromPoint, rangeFromPoint := uiaPointThunks()
	uiaFillVtable(v.simple[:], v.unknown, uiaProviderOptions, uiaGetPatternProvider, uiaGetPropertyValue, uiaHostProvider)
	uiaFillVtable(v.fragment[:], v.unknown, uiaNavigate, uiaGetRuntimeID, uiaBoundingRectangle, uiaEmbeddedRoots, uiaSetFocus, uiaFragmentRoot)
	uiaFillVtable(v.root[:], v.unknown, nil, uiaGetFocus)
	v.root[3] = elementFromPoint
	uiaFillVtable(v.invoke[:], v.unknown, uiaInvoke)
	uiaFillVtable(v.value[:], v.unknown, uiaSetValue, uiaGetValue, uiaIsReadOnly)
	uiaFillVtable(v.toggle[:], v.unknown, uiaToggle, uiaGetToggleState)
	uiaFillVtable(v.selection[:], v.unknown, uiaSelect, uiaAddToSelection, uiaRemoveFromSelection, uiaIsSelected, uiaSelectionContainer)
	uiaFillVtable(v.expand[:], v.unknown, uiaExpandNode, uiaCollapseNode, uiaGetExpandState)
	uiaFillVtable(v.scroll[:], v.unknown, uiaScrollIntoView)
	uiaFillVtable(v.text[:], v.unknown, uiaTextSelection, uiaTextVisibleRanges, uiaRangeFromChild, nil, uiaDocumentRange, uiaSupportedTextSelection)
	v.text[6] = rangeFromPoint
	uiaFillVtable(v.textRange[:], v.unknown, uiaRangeClone, uiaRangeCompare, uiaRangeCompareEndpoints, uiaRangeExpand, uiaRangeFindAttribute,
		uiaRangeFindText, uiaRangeAttribute, uiaRangeBounds, uiaRangeEnclosingElement, uiaRangeGetText, uiaRangeMove, uiaRangeMoveEndpointByUnit,
		uiaRangeMoveEndpointByRange, uiaRangeSelect, uiaRangeAddToSelection, uiaRangeRemoveFromSelection, uiaRangeScrollIntoView, uiaRangeChildren)
	uiaFillVtable(v.grid[:], v.unknown, uiaGridGetItem, uiaGridRowCount, uiaGridColumnCount)
	uiaFillVtable(v.gridItem[:], v.unknown, uiaGridItemRow, uiaGridItemColumn, uiaGridItemRowSpan, uiaGridItemColumnSpan, uiaGridItemContainer)
	uiaFillVtable(v.table[:], v.unknown, uiaTableHeaders(RoleRowHeader), uiaTableHeaders(RoleColumnHeader), uiaTableMajor)
	uiaFillVtable(v.tableItem[:], v.unknown, uiaTableItemHeaders(RoleRowHeader), uiaTableItemHeaders(RoleColumnHeader))
	v.subclass = syscall.NewCallback(uiaSubclassProc)
}

func uiaFillVtable(slots []uintptr, unknown [3]uintptr, methods ...any) {
	copy(slots, unknown[:])
	for i, m := range methods {
		if m != nil {
			slots[3+i] = syscall.NewCallback(m)
		}
	}
}

var (
	uiaElementFromPointCallback uintptr //nolint:unused // loaded by the native UIA point thunk in assembly
	uiaRangeFromPointCallback   uintptr //nolint:unused // loaded by the native UIA point thunk in assembly
)

func uiaPointThunks() (element, rangeFrom uintptr)
