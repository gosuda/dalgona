package figure

import (
	"bufio"
	"encoding/binary"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"testing"
	"time"
)

var testFonts = sync.OnceValue(func() Fonts {
	_, source, _, _ := runtime.Caller(0)
	read := func(name string) []byte {
		data, err := os.ReadFile(filepath.Join(filepath.Dir(source), "..", "platform", "fontfaces", name))
		if err != nil {
			panic(err)
		}
		return data
	}
	return Fonts{Math: read("STIXTwoMath-Regular.otf"), Text: [][]byte{read("HankenGrotesk-400.ttf"), read("Pretendard-400.otf")}}
})

const (
	testChildWrongNonce = "--test-figure-child-wrong-nonce"
	testChildOversized  = "--test-figure-child-oversized-frame"
	testChildSilent     = "--test-figure-child-silent"
	testChildDeaf       = "--test-figure-child-deaf"
	testChildResident   = "--test-figure-child-resident"
)

func TestMain(m *testing.M) {
	if len(os.Args) == 2 {
		switch os.Args[1] {
		case ChildArgument:
			os.Exit(ServeChild(testFonts()))
		case testChildWrongNonce, testChildOversized, testChildSilent:
			os.Exit(misbehavingChild(os.Args[1]))
		case testChildDeaf:
			os.Exit(deafChild())
		case testChildResident:
			os.Exit(residentChild())
		}
	}
	os.Exit(m.Run())
}

func misbehavingChild(mode string) int {
	in := bufio.NewReader(os.Stdin)
	launch, _, err := readFrame(in, frameHeader+EnvelopeLimit)
	if err != nil {
		return 2
	}
	nonce := launch.Nonce
	if mode == testChildWrongNonce {
		if len(nonce) >= 2 && nonce[:2] == "00" {
			nonce = "ff" + nonce[2:]
		} else {
			nonce = "00" + nonce[2:]
		}
	}
	hello := &envelope{Type: messageHello, Version: ProtocolVersion, Engines: engineManifest(), Fonts: testFonts().manifest(), Nonce: nonce}
	if writeFrame(os.Stdout, hello, nil, ReplyFrameLimit) != nil {
		return 1
	}
	if _, _, err := readFrame(in, requestFrameLimit); err != nil {
		return 2
	}
	if mode == testChildOversized {
		var header [frameHeader]byte
		binary.LittleEndian.PutUint32(header[:4], 1<<30)
		binary.LittleEndian.PutUint16(header[4:], 16)
		_, _ = os.Stdout.Write(header[:])
	}
	time.Sleep(time.Hour)
	return 0
}

func deafChild() int {
	in := bufio.NewReader(os.Stdin)
	launch, _, err := readFrame(in, frameHeader+EnvelopeLimit)
	if err != nil {
		return 2
	}
	hello := &envelope{Type: messageHello, Version: ProtocolVersion, Engines: engineManifest(), Fonts: testFonts().manifest(), Nonce: launch.Nonce}
	if writeFrame(os.Stdout, hello, nil, ReplyFrameLimit) != nil {
		return 1
	}
	time.Sleep(time.Hour)
	return 0
}
