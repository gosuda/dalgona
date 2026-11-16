package main

import "fmt"

type greeter struct {
	words []string
}

func newGreeter() greeter {
	return greeter{words: []string{"hello"}}
}

func (g greeter) text() string {
	return g.words[0]
}

func main() {
	g := newGreeter()
	fmt.Println(g.text())
}
