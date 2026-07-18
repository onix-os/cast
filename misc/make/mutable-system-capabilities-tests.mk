.PHONY: forge-mutable-system-capabilities-test

forge-mutable-system-capabilities-test: forge-mutable-startup-namespace-test
	@set -euo pipefail; \
	capabilities=crates/forge/src/client/mutable_system_capabilities.rs; \
	capability_tests=crates/forge/src/client/mutable_system_capabilities/tests.rs; \
	construction=crates/forge/src/client/core/construction.rs; \
	client_model=crates/forge/src/client/core/client_model.rs; \
	client_module=crates/forge/src/client/mod.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	boot=crates/forge/src/client/boot.rs; \
	test_name='client::mutable_system_capabilities::tests::production_capabilities_keep_install_state_layout_and_root_coherent_across_two_roots'; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -Fqx "$$test_name: test" <<<"$$listed"; \
	aggregate_fields="$$( timeout 10s awk '/^pub\(super\) struct MutableSystemCapabilities \{$$/ { inside = 1; next } inside && /^}/ { exit } inside && /^[[:space:]]+[a-z_]+:/ { field = $$1; sub(/:.*/, "", field); print field }' "$$capabilities" )"; \
	timeout 10s test "$$aggregate_fields" = $$'install_db\nstate_db\nlayout_db\ninstallation'; \
	client_fields="$$( timeout 10s awk '/^pub struct Client \{$$/ { inside = 1; next } inside && /^}/ { exit } inside && /^[[:space:]]+[a-z_]+:/ { field = $$1; sub(/:.*/, "", field); print field }' "$$client_model" )"; \
	timeout 10s test "$$client_fields" = $$'registry\ninstall_db\nstate_db\nlayout_db\nconfig\nrepositories\nscope\ninstallation'; \
	timeout 10s test "$$( timeout 10s grep -Fc 'pub(super) fn open_mutable_system_capabilities(' "$$capabilities" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'let mut system = open_mutable_system_capabilities(self.installation)?;' "$$construction" )" = 1; \
	production_openers="$$( timeout 10s rg -n -g '*.rs' -g '!**/tests.rs' -g '!*_tests.rs' -g '!*_test_support.rs' -g '!**/test_support.rs' 'open_mutable_system_capabilities\(' crates/forge/src/client | timeout 10s wc -l )"; \
	timeout 10s test "$$production_openers" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'installation.mutable_database_location(' "$$capabilities" )" = 3; \
	for kind in Install State Layout; do \
		timeout 10s test "$$( timeout 10s grep -Fc "installation::DatabaseKind::$$kind" "$$capabilities" )" = 2; \
	done; \
	timeout 10s awk 'BEGIN { expected[1] = "installation.revalidate_mutable_namespace()?;"; expected[2] = "let install = installation.mutable_database_location(installation::DatabaseKind::Install)?;"; expected[3] = "let state = installation.mutable_database_location(installation::DatabaseKind::State)?;"; expected[4] = "let layout = installation.mutable_database_location(installation::DatabaseKind::Layout)?;"; expected[5] = "let (install_url, install_anchor) = install.parts();"; expected[6] = "let install_db = db::meta::Database::new_mutable_system_anchored(install_url, install_anchor);"; expected[7] = "after_system_database_open(installation::DatabaseKind::Install);"; expected[8] = "let alias = install.revalidate();"; expected[9] = "let namespace = installation.revalidate_mutable_namespace();"; expected[10] = "namespace?;"; expected[11] = "alias?;"; expected[12] = "let install_db = install_db?;"; expected[13] = "let (state_url, state_anchor) = state.parts();"; expected[14] = "let state_db = db::state::Database::new_anchored(state_url, state_anchor);"; expected[15] = "after_system_database_open(installation::DatabaseKind::State);"; expected[16] = "let alias = state.revalidate();"; expected[17] = "let namespace = installation.revalidate_mutable_namespace();"; expected[18] = "namespace?;"; expected[19] = "alias?;"; expected[20] = "let state_db = state_db?;"; expected[21] = "let (layout_url, layout_anchor) = layout.parts();"; expected[22] = "let layout_db = db::layout::Database::new_anchored(layout_url, layout_anchor);"; expected[23] = "after_system_database_open(installation::DatabaseKind::Layout);"; expected[24] = "let alias = layout.revalidate();"; expected[25] = "let namespace = installation.revalidate_mutable_namespace();"; expected[26] = "namespace?;"; expected[27] = "alias?;"; expected[28] = "let layout_db = layout_db?;"; expected[29] = "installation.revalidate_mutable_namespace()?;"; next_expected = 1 } { line = $$0; sub(/^[[:space:]]+/, "", line); if (next_expected <= 29 && line == expected[next_expected]) next_expected += 1 } END { exit !(next_expected == 30) }' "$$capabilities"; \
	if timeout 10s grep -Fq 'mutable_database_location(' "$$construction"; then \
		timeout 10s printf '%s\n' 'Client construction reopened a loose mutable-system database capability' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s rg -U -q '#\[cfg\(test\)\]\n    pub\(in crate::client\) fn from_test_parts\(\n        _seal: &MutableSystemCapabilitiesTestSeal,' "$$capabilities"; \
	timeout 10s rg -U -q '#\[cfg\(test\)\]\npub\(in crate::client\) struct MutableSystemCapabilitiesTestSeal \{' "$$capabilities"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct MutableSystemCapabilitiesTestSeal {' "$$capabilities"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'db::meta::Database::new(":memory:")' "$$capabilities" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Ec 'db::(meta|state|layout)::Database::new\(' "$$capabilities" )" = 1; \
	timeout 10s test "$$( timeout 10s sed -n '/fn from_test_parts(/,/^    }/p' "$$capabilities" | timeout 10s grep -Fc 'db::meta::Database::new(":memory:")' )" = 1; \
	if timeout 10s rg -n 'db_path\(' "$$capabilities"; then \
		timeout 10s printf '%s\n' 'Mutable-system capabilities reopened a database through a public path' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	for file in $$( timeout 10s rg -l 'MutableSystemCapabilities::from_test_parts\(|MutableSystemCapabilitiesTestSeal::new\(' crates/forge/src/client -g '*.rs' ); do \
		case "$$file" in */tests/*|*_tests/*|*_test_support.rs|*/test_support.rs) ;; \
		*) timeout 10s printf 'test-only mutable-system constructor called by production path: %s\n' "$$file" >&2; exit 1 ;; \
		esac; \
	done; \
	timeout 10s grep -Fqx 'mod mutable_system_capabilities;' "$$client_module"; \
	timeout 10s grep -Fqx 'use mutable_system_capabilities::{MutableSystemCapabilities, open_mutable_system_capabilities};' "$$client_module"; \
	if timeout 10s rg -n 'pub\(crate\) use mutable_system_capabilities|installation_mut|into_client_parts' "$$client_module" "$$capabilities" "$$construction"; then \
		timeout 10s printf '%s\n' 'Mutable-system capability opacity or consuming construction was widened' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s rg -U -n '#\[derive\([^]]*(Clone|Copy|Default)[^]]*\)\][[:space:]]*\npub\(super\) struct MutableSystemCapabilities|impl (Clone|Copy|Default) for MutableSystemCapabilities' "$$capabilities"; then \
		timeout 10s printf '%s\n' 'Mutable-system capabilities became duplicable or synthesizable' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s rg -n -g '*.rs' -g '!mutable_system_capabilities.rs' '(^|[=(,])[[:space:]]*MutableSystemCapabilities[[:space:]]*\{' crates/forge/src/client; then \
		timeout 10s printf '%s\n' 'Mutable-system capability struct literal escaped its defining module' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s rg -U -q 'pub\(super\) fn enter\(\n        system: &MutableSystemCapabilities,\n        active_state_reservation: &ActiveStateReservation,\n    \) -> Result<Self, Error> \{' "$$startup_gate"; \
	timeout 10s awk 'index($$0, "ActiveStateReservation::acquire()?") { reservation = NR } index($$0, "open_mutable_system_capabilities(self.installation)?") { capabilities = NR } index($$0, "CleanSystemStartup::enter(&system, &active_state_reservation)") { enter = NR } END { exit !(reservation && capabilities && enter && reservation < capabilities && capabilities < enter) }' "$$construction"; \
	timeout 10s test "$$( timeout 10s rg -n 'system\.into_client\(' crates/forge/src/client -g '*.rs' | timeout 10s wc -l )" = 1; \
	timeout 10s awk 'index($$0, "drop(startup_gate);") { gate = NR } index($$0, "drop(active_state);") { active = NR } index($$0, "system.into_client(") { consume = NR; count += 1 } END { exit !(gate && active && consume && gate < active && active < consume && count == 1) }' "$$construction"; \
	timeout 10s awk '/^[[:space:]]*pub(\([^)]*\))?[[:space:]]+fn[[:space:]]/ { signature = $$0; while (signature !~ /\)[[:space:]]*(->[^{]+)?\{/ && getline > 0) signature = signature "\n" $$0; typed = signature ~ /Installation/ && signature ~ /db::state::Database/ && signature ~ /db::layout::Database/; named = signature ~ /installation[[:space:]]*:/ && signature ~ /(state_db|state_database)[[:space:]]*:/ && signature ~ /(layout_db|layout_database)[[:space:]]*:/; if (typed || named) { print signature > "/dev/stderr"; bad = 1 } } END { exit bad }' "$$boot"; \
	for file in "$$capabilities" "$$capability_tests" "$$construction" "$$client_model" "$$client_module" "$$startup_gate" "$$boot" misc/make/mutable-system-capabilities-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 600s $(CARGO) test -p forge --lib "$$test_name" -- --exact --test-threads=1
