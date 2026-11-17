package agent

import (
	"fmt"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/agent/prompt"
)

type requestContext struct {
	systemBase        string
	memory            string
	memoryGeneration  uint64
	baseGeneration    uint64
	dynamicThroughSeq uint64
}

func newRequestContext(state event.State, bound event.PromptBound, head uint64) (requestContext, error) {
	base, ok := state.PromptBases[bound.BaseGeneration]
	if !ok {
		return requestContext{}, fmt.Errorf("agent: prompt base generation %d is not recorded", bound.BaseGeneration)
	}
	memory, ok := state.MemoryVersions[bound.MemoryGeneration]
	if !ok && bound.MemoryGeneration != 0 {
		return requestContext{}, fmt.Errorf("agent: memory generation %d is not recorded", bound.MemoryGeneration)
	}
	return requestContext{
		systemBase: base, memory: memory, memoryGeneration: bound.MemoryGeneration,
		baseGeneration: bound.BaseGeneration, dynamicThroughSeq: head,
	}, nil
}

func (rc requestContext) system() string {
	return prompt.AppendMemory(rc.systemBase, rc.memory)
}

func (rc requestContext) through(head uint64) requestContext {
	rc.dynamicThroughSeq = head
	return rc
}

func (w *worker) snapshot(v view) requestContext {
	return w.requestContext.through(v.head())
}

func (a *actor) currentBase() string {
	return a.state.PromptBases[a.state.CurrentBaseGeneration]
}

func (a *actor) currentMemory() string {
	return a.state.MemoryVersions[a.state.CurrentMemoryGeneration]
}

func (a *actor) currentContext() requestContext {
	return requestContext{
		systemBase: a.currentBase(), memory: a.currentMemory(),
		baseGeneration: a.state.CurrentBaseGeneration, memoryGeneration: a.state.CurrentMemoryGeneration,
		dynamicThroughSeq: a.head(),
	}
}
