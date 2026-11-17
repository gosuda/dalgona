package main

import (
	"fmt"
	"io"
	"os"
	"strings"
)

func main() {
	fmt.Fprintf(os.Stderr, "args: %s\n", strings.Join(os.Args[1:], " "))
	if _, err := io.WriteString(os.Stdout, `{"method":"echo/ready"}`+"\n"); err != nil {
		os.Exit(1)
	}
	if _, err := io.Copy(os.Stdout, os.Stdin); err != nil {
		fmt.Fprintf(os.Stderr, "echo: %v\n", err)
		os.Exit(1)
	}
	os.Exit(3)
}
