package main

import (
	"testing"

	"fixtures.invalid/cast/go-module-fixture/internal/application"
)

func TestSelfTestIdentity(t *testing.T) {
	if got := application.Identity(); got != selfTestIdentity {
		t.Fatalf("Identity() = %q, want %q", got, selfTestIdentity)
	}
}
