.PHONY: container-private-device-test

# The pure contract tests always run.  Set
# CONTAINER_REQUIRE_PRIVATE_DEVICE_PROVISIONING=1 only in a disposable VM to
# turn a missing initial-user-namespace mount/device capability into a hard
# failure and exercise the real detached-device provider.
container-private-device-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p container --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^private_devices::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 7; \
	timeout 300s $(CARGO) test -p container --lib 'private_devices::tests::' -- --test-threads=1
