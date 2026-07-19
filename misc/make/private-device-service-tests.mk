.PHONY: private-device-service-test

private-device-service-test: cast-private-device-broker-entrypoint-test
	@set -eu; \
	socket=misc/systemd/cast-private-devices.socket; \
	service=misc/systemd/cast-private-devices@.service; \
	timeout 10s test -f "$$socket"; \
	timeout 10s test -f "$$service"; \
	timeout 10s grep -Fqx 'ListenSequentialPacket=/run/cast/private-devices.socket' "$$socket"; \
	timeout 10s grep -Fqx 'Accept=yes' "$$socket"; \
	status=0; timeout 10s grep -Eq '^Service=' "$$socket" || status=$$?; \
	timeout 10s test "$$status" = 1; \
	timeout 10s grep -Fqx 'SocketMode=0666' "$$socket"; \
	timeout 10s grep -Fqx 'Backlog=32' "$$socket"; \
	timeout 10s grep -Fqx 'MaxConnections=32' "$$socket"; \
	timeout 10s grep -Fqx 'MaxConnectionsPerSource=4' "$$socket"; \
	timeout 10s grep -Fqx 'TriggerLimitIntervalSec=1s' "$$socket"; \
	timeout 10s grep -Fqx 'TriggerLimitBurst=32' "$$socket"; \
	timeout 10s grep -Fqx 'PollLimitIntervalSec=1s' "$$socket"; \
	timeout 10s grep -Fqx 'PollLimitBurst=16' "$$socket"; \
	timeout 10s grep -Fqx 'Type=exec' "$$service"; \
	timeout 10s grep -Fqx 'ExecStart=/usr/bin/cast --private-device-broker' "$$service"; \
	timeout 10s grep -Fqx 'User=root' "$$service"; \
	timeout 10s grep -Fqx 'Group=root' "$$service"; \
	timeout 10s grep -Fqx 'StandardInput=socket' "$$service"; \
	timeout 10s grep -Fqx 'StandardOutput=journal' "$$service"; \
	timeout 10s grep -Fqx 'CapabilityBoundingSet=CAP_SYS_ADMIN CAP_MKNOD' "$$service"; \
	timeout 10s test "$$(timeout 10s grep -c '^CapabilityBoundingSet=' "$$service")" = 1; \
	timeout 10s grep -Fqx 'NoNewPrivileges=yes' "$$service"; \
	timeout 10s grep -Fqx 'RestrictAddressFamilies=AF_UNIX' "$$service"; \
	timeout 10s test "$$(timeout 10s grep -c '^RestrictAddressFamilies=' "$$service")" = 1; \
	timeout 10s grep -Fqx 'PrivateMounts=yes' "$$service"; \
	timeout 10s grep -Fqx 'RuntimeMaxSec=5s' "$$service"; \
	timeout 10s grep -Fqx 'TasksMax=1' "$$service"; \
	timeout 10s grep -Fqx 'MemoryMax=128M' "$$service"; \
	timeout 10s grep -Fqx 'LimitNOFILE=32' "$$service"; \
	status=0; timeout 10s grep -Eq '^(AmbientCapabilities|PrivateDevices|PrivateUsers|DevicePolicy)=' "$$service" || status=$$?; \
	timeout 10s test "$$status" = 1; \
	status=0; timeout 10s grep -q 'SPDX-' "$$socket" "$$service" || status=$$?; \
	timeout 10s test "$$status" = 1; \
	tmp="$$(timeout 10s mktemp -d)"; \
	trap 'timeout 10s rm -rf -- "$$tmp"' EXIT HUP INT TERM; \
	timeout 10s mkdir -p "$$tmp/etc/systemd/system" "$$tmp/usr/bin"; \
	timeout 10s install -m 0644 "$$socket" "$$tmp/etc/systemd/system/cast-private-devices.socket"; \
	timeout 10s install -m 0644 "$$service" "$$tmp/etc/systemd/system/cast-private-devices@.service"; \
	timeout 10s install -m 0755 /bin/true "$$tmp/usr/bin/cast"; \
	timeout 30s systemd-analyze --root="$$tmp" verify --recursive-errors=no \
		cast-private-devices.socket cast-private-devices@.service

.PHONY: cast-private-device-broker-entrypoint-test

cast-private-device-broker-entrypoint-test:
	@set -eu; \
	listed="$$( timeout 120s $(CARGO) test -p cast --lib -- --list )"; \
	for test in \
		tests::private_device_broker_mode_is_hidden_and_not_a_subcommand \
		tests::broker_standard_input_duplicate_is_owned_and_close_on_exec; do \
		timeout 10s printf '%s\n' "$$listed" | timeout 10s grep -Fqx "$$test: test"; \
		timeout 120s $(CARGO) test -p cast --lib "$$test" -- --exact --test-threads=1; \
	done; \
	test=private_device_broker_mode_rejects_non_socket_standard_input; \
	listed="$$( timeout 120s $(CARGO) test -p cast --test cli -- --list )"; \
	timeout 10s printf '%s\n' "$$listed" | timeout 10s grep -Fqx "$$test: test"; \
	timeout 120s $(CARGO) test -p cast --test cli "$$test" -- --exact --test-threads=1
