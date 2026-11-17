package model

import (
	"context"
	"io"
	"sync"
	"testing"
	"time"
)

func endlessSSE() io.ReadCloser {
	reader, writer := io.Pipe()
	go func() {
		line := []byte("data: {\"delta\":\"x\"}\n\n")
		for {
			if _, err := writer.Write(line); err != nil {
				return
			}
		}
	}()
	return reader
}

func TestInterruptedSSEStreamJoinsReaderBeforeFinish(t *testing.T) {
	timer := time.NewTimer(5 * time.Second)
	defer timer.Stop()
	for round := range 200 {
		ctx, cancel := context.WithCancel(t.Context())
		stream := NewEventStream(ctx, 1)
		handled := 0
		finished := make(chan int)
		go func() {
			_ = ReadSSEWithTimeouts(stream.Context(ctx), endlessSSE(), SSEOptions{Control: stream.Timers()}, func(_, _ []byte) error {
				handled++
				stream.Emit(TextDelta{Text: "x"})
				return nil
			})
			seen := handled
			stream.Finish(nil)
			finished <- seen
		}()
		for range round % 7 {
			<-stream.Events()
		}
		cancel()
		for range stream.Events() {
		}
		if !timer.Stop() {
			select {
			case <-timer.C:
			default:
			}
		}
		timer.Reset(5 * time.Second)
		select {
		case <-finished:
		case <-timer.C:
			t.Fatalf("round %d: stream did not finish after cancel", round)
		}
	}
}

func TestEmitDuringFinishNeverPanics(t *testing.T) {
	for range 500 {
		stream := NewEventStream(t.Context(), 1)
		var emitters sync.WaitGroup
		for range 4 {
			emitters.Go(func() {
				for range 8 {
					stream.Emit(TextDelta{Text: "x"})
				}
			})
		}
		stream.Finish(nil)
		emitters.Wait()
		for range stream.Events() {
		}
		if stream.Emit(TextDelta{Text: "late"}) {
			t.Fatal("Emit after Finish reported delivery")
		}
	}
}
