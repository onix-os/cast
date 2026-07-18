package main

import (
	"fmt"
	"os"

	"fixtures.invalid/cast/go-module-fixture/internal/application"
)

const selfTestIdentity = "cast go module fixture: vendored dependency v0.1.0: declarative userspace"

func main() {
	if len(os.Args) == 2 && os.Args[1] == "--self-test" {
		if application.Identity() != selfTestIdentity {
			os.Exit(1)
		}
		fmt.Println(selfTestIdentity)
		return
	}
	if len(os.Args) != 1 {
		fmt.Fprintln(os.Stderr, "usage: cast-go-module-fixture [--self-test]")
		os.Exit(64)
	}
	fmt.Println(application.Identity())
}
