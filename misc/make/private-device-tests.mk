.PHONY: container-private-device-test

# The pure contract tests always run.  Set
# CONTAINER_REQUIRE_PRIVATE_DEVICE_PROVISIONING=1 only in a disposable VM to
# turn a missing initial-user-namespace mount/device capability into a hard
# failure and exercise the real detached-device provider.
container-private-device-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p container --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	provider_count="$$( timeout 10s grep -c '^private_devices::tests::.*: test$$' <<<"$$listed" )"; \
	broker_count="$$( timeout 10s grep -c '^private_device_broker::tests::.*: test$$' <<<"$$listed" )"; \
	assembly_count="$$( timeout 10s grep -c '^private_device_assembly::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$provider_count" = 8; \
	timeout 10s test "$$broker_count" = 9; \
	timeout 10s test "$$assembly_count" = 7; \
	for prefix in \
		private_devices::tests:: \
		private_device_assembly::tests:: \
		private_device_broker::tests::; do \
		timeout 300s $(CARGO) test -p container --lib "$$prefix" -- --test-threads=1; \
	done; \
	assembly="$(TOP_DIR)/crates/container/src/private_device_assembly.rs"; \
	provider="$(TOP_DIR)/crates/container/src/private_devices.rs"; \
	activation="$(TOP_DIR)/crates/container/src/activation.rs"; \
	pseudo="$(TOP_DIR)/crates/container/src/mounts/pseudo_filesystems.rs"; \
	anchored="$(TOP_DIR)/crates/container/src/mounts/anchored_root.rs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$assembly" )" -le 1000; \
	timeout 10s test "$$( timeout 10s grep -c '\.validate_namespace_invariants()' "$$assembly" )" = 2; \
	if timeout 10s rg -U -n 'devices\s*\.validate\(\)' "$$assembly"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'mount_setattr(read-only, nonrecursive)' "$$assembly"; \
	timeout 10s grep -Fq 'self.finish_attached()' "$$assembly"; \
	timeout 10s grep -Fq 'move_mount parent onto authenticated final target' "$$assembly"; \
	timeout 10s grep -Fq 'device_path(device)' "$$assembly"; \
	timeout 10s grep -Fq 'require_no_parent_inode_headroom(self.parent.as_raw_fd())?;' "$$assembly"; \
	seal="$$( timeout 10s sed -n '/^fn seal_parent_read_only(/,/^}/p' "$$assembly" )"; \
	timeout 10s grep -Fq 'AT_EMPTY_PATH as usize' <<<"$$seal"; \
	if timeout 10s rg -n 'AT_RECURSIVE' <<<"$$seal"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'scratch.seal_root()?;' "$$provider"; \
	timeout 10s grep -Fq 'observation.links != 1' "$$provider"; \
	timeout 10s grep -Fq 'observation.tmpfs_free_inodes != 0' "$$provider"; \
	if timeout 10s rg -n 'unlink_all|unlinkat' "$$provider"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'PseudoMountDecision::PrivateMinimalDev' "$$pseudo" "$$anchored"; \
	timeout 10s grep -Fq 'let mounts = acquire_private_devices_from_broker()?;' "$$activation"; \
	timeout 10s grep -Fq 'drop(self.private_devices.take());' "$$activation"; \
	if timeout 10s rg -n 'provision_private_device_mounts|geteuid' "$$activation"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'MINIMAL_DEV_IDENTITIES|prepare_anchored_minimal_dev|mount_minimal_dev|bind_minimal_device|/old_root/dev' "$$pseudo" "$$anchored"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi
