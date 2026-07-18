package application

import "testing"

func TestIdentityUsesVendoredModule(t *testing.T) {
	const want = "cast go module fixture: vendored dependency v0.1.0: declarative userspace"
	if got := Identity(); got != want {
		t.Fatalf("Identity() = %q, want %q", got, want)
	}
}
