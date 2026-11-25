//go:build windows && (amd64 || arm64)

package platform

import (
	"slices"
	"sync"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	comClassContextLocalServer = 0x4
	comRegisterMultipleUse     = 0x1
	comNoAggregation           = 0x80040110
)

var (
	comOle32                          = windows.NewLazySystemDLL("ole32.dll")
	procCoRegisterClassObject         = comOle32.NewProc("CoRegisterClassObject")
	procCoRevokeClassObject           = comOle32.NewProc("CoRevokeClassObject")
	iidClassFactory                   = winrtGUID{0x00000001, 0x0000, 0x0000, [8]byte{0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46}}
	iidNotificationActivationCallback = winrtGUID{0x53e31837, 0x6600, 0x4a81, [8]byte{0x93, 0x95, 0x75, 0xcf, 0xfe, 0x74, 0x6f, 0x94}}
	toastActivatorSlots               [4]uintptr
	toastFactorySlots                 [5]uintptr
	toastActivatorObject              = struct{ vtbl *[4]uintptr }{&toastActivatorSlots}
	toastFactoryObject                = struct{ vtbl *[5]uintptr }{&toastFactorySlots}
	toastActivatorVtables             = sync.OnceFunc(func() {
		addRef, release := syscall.NewCallback(comStaticAddRef), syscall.NewCallback(comStaticRelease)
		toastActivatorSlots = [4]uintptr{syscall.NewCallback(toastActivatorQueryInterface), addRef, release, syscall.NewCallback(toastActivatorActivate)}
		toastFactorySlots = [5]uintptr{syscall.NewCallback(toastFactoryQueryInterface), addRef, release, syscall.NewCallback(toastFactoryCreateInstance), syscall.NewCallback(toastFactoryLockServer)}
	})
	toastActivationTarget struct {
		sync.Mutex
		notifier *Notifier
	}
)

func (s *toastSession) listen() error {
	toastActivatorVtables()
	toastActivationTarget.Lock()
	toastActivationTarget.notifier = s.owner
	toastActivationTarget.Unlock()
	hr, _, _ := procCoRegisterClassObject.Call(
		uintptr(unsafe.Pointer(&clsidToastActivator)), //nolint:gosec // CoRegisterClassObject reads the CLSID
		uintptr(unsafe.Pointer(&toastFactoryObject)),  //nolint:gosec // the package-level class factory outlives its registration
		comClassContextLocalServer,
		comRegisterMultipleUse,
		uintptr(unsafe.Pointer(&s.activator)), //nolint:gosec // CoRegisterClassObject writes the registration cookie
	)
	if winrtFailed(hr) {
		s.forgetTarget()
		return winrtError("register the notification activator "+toastActivatorID+" so a click can reopen dalgona; restart dalgona", hr)
	}
	s.listening = true
	return nil
}

func (s *toastSession) unlisten() {
	if !s.listening {
		return
	}
	_, _, _ = procCoRevokeClassObject.Call(uintptr(s.activator))
	s.listening, s.activator = false, 0
	s.forgetTarget()
}

func (s *toastSession) forgetTarget() {
	toastActivationTarget.Lock()
	if toastActivationTarget.notifier == s.owner {
		toastActivationTarget.notifier = nil
	}
	toastActivationTarget.Unlock()
}

func comStaticAddRef(uintptr) uintptr { return 2 }

func comStaticRelease(uintptr) uintptr { return 1 }

func comInterface(iid *winrtGUID, out *uintptr, object unsafe.Pointer, own *winrtGUID) uintptr {
	if out == nil || iid == nil {
		return uintptr(windows.E_POINTER)
	}
	if *iid != iidUnknown && *iid != *own {
		*out = 0
		return uintptr(windows.E_NOINTERFACE)
	}
	*out = uintptr(object)
	return uintptr(windows.S_OK)
}

func toastFactoryQueryInterface(_ uintptr, iid *winrtGUID, out *uintptr) uintptr {
	return comInterface(iid, out, unsafe.Pointer(&toastFactoryObject), &iidClassFactory) //nolint:gosec // the package-level class factory never moves
}

func toastFactoryCreateInstance(_, outer uintptr, iid *winrtGUID, out *uintptr) uintptr {
	if outer != 0 {
		if out != nil {
			*out = 0
		}
		return comNoAggregation
	}
	return toastActivatorQueryInterface(0, iid, out)
}

func toastFactoryLockServer(_, _ uintptr) uintptr {
	return uintptr(windows.S_OK)
}

func toastActivatorQueryInterface(_ uintptr, iid *winrtGUID, out *uintptr) uintptr {
	return comInterface(iid, out, unsafe.Pointer(&toastActivatorObject), &iidNotificationActivationCallback) //nolint:gosec // the package-level activator never moves
}

func toastActivatorActivate(_ uintptr, app, launch *uint16, _, _ uintptr) uintptr {
	appUnits, appTerminated := comUnits(app)
	launchUnits, launchTerminated := comUnits(launch)
	if !appTerminated || !launchTerminated {
		return uintptr(windows.E_INVALIDARG)
	}
	tag, ok := toastActivation(appUnits, launchUnits)
	if !ok {
		return uintptr(windows.E_INVALIDARG)
	}
	toastActivationTarget.Lock()
	n := toastActivationTarget.notifier
	toastActivationTarget.Unlock()
	if n != nil {
		n.clicked.fromActivator(tag)
		n.activate(tag)
	}
	return uintptr(windows.S_OK)
}

func comUnits(text *uint16) ([]uint16, bool) {
	if text == nil {
		return nil, false
	}
	units := unsafe.Slice(text, toastLaunchUnits+1) //nolint:gosec // COM supplies a NUL-terminated UTF-16 string and the scan stops at its terminator
	end := slices.Index(units, 0)
	if end < 0 {
		return nil, false
	}
	return units[:end], true
}
