def exact_keys($expected):
  type == "object" and keys_unsorted == $expected;

def positive_integer:
  type == "number" and . > 0 and . == floor;

def bounded_positive_integer($maximum):
  positive_integer and . <= $maximum;

def lowercase_sha256:
  type == "string" and test("^[0-9a-f]{64}$");

def safe_artifact_name:
  type == "string"
  and utf8bytelength <= 255
  and test("^[A-Za-z0-9][A-Za-z0-9._+-]*$");

def valid_plan_observation:
  exact_keys(["byte_count", "sha256", "derivation_id"])
  and (.byte_count | bounded_positive_integer(16777216))
  and (.sha256 | lowercase_sha256)
  and (.derivation_id | lowercase_sha256)
  and .sha256 == .derivation_id;

def valid_build_lock_observation($write_outcome):
  exact_keys(["write_outcome", "byte_count", "sha256"])
  and .write_outcome == $write_outcome
  and (.byte_count | bounded_positive_integer(1048576))
  and (.sha256 | lowercase_sha256);

def valid_artifact_entry:
  exact_keys(["name", "kind", "byte_count", "sha256"])
  and (.name | safe_artifact_name)
  and (.byte_count | bounded_positive_integer(134217728))
  and (.sha256 | lowercase_sha256)
  and (
    if .kind == "stone" then
      (.name | endswith(".stone"))
      and .name != "manifest.x86_64.bin"
      and .name != "manifest.x86_64.jsonc"
    elif .kind == "manifest-bin" then
      .name == "manifest.x86_64.bin"
    elif .kind == "manifest-jsonc" then
      .name == "manifest.x86_64.jsonc"
    else
      false
    end
  );

def expected_stone_count($fixture):
  if [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "factory-override",
    "hooks-patch",
    "meson",
    "multiple-sources",
    "post-install-smoke-test"
  ] | index($fixture) then 9
  elif $fixture == "header-only-library" then 2
  elif $fixture == "daemon-generated" then 3
  elif $fixture == "generated-config" then 1
  elif $fixture == "generated-shell" then 1
  elif $fixture == "desktop-integration" then 1
  elif $fixture == "external-test-vectors" then 2
  elif $fixture == "font-family" then 1
  elif $fixture == "gettext-localization" then 1
  elif $fixture == "go-module" then 1
  elif $fixture == "pgo-workload" then 1
  elif $fixture == "python-module" then 1
  elif $fixture == "relation-policy" then 1
  elif $fixture == "plugin-output" then 3
  elif $fixture == "split" then 5
  elif $fixture == "system-integration-assets" then 1
  elif $fixture == "userspace-profile" then 1
  else -1
  end;

def valid_artifacts($fixture):
  . as $artifacts
  | (expected_stone_count($fixture)) as $expected_stones
  | exact_keys([
      "stone_count",
      "manifest_count",
      "artifact_count",
      "total_bytes",
      "ledger_sha256",
      "entries"
    ])
    and $artifacts.stone_count == $expected_stones
    and $artifacts.manifest_count == 2
    and $artifacts.artifact_count == ($expected_stones + 2)
    and ($artifacts.total_bytes | bounded_positive_integer(268435456))
    and ($artifacts.ledger_sha256 | lowercase_sha256)
    and ($artifacts.entries | type == "array")
    and ($artifacts.entries | length) == $artifacts.artifact_count
    and all($artifacts.entries[]; valid_artifact_entry)
    and (
      $artifacts.entries
      | map(.name) as $names
      | $names == ($names | sort)
        and ($names | unique | length) == ($names | length)
    )
    and ($artifacts.entries | map(select(.kind == "stone")) | length) == $artifacts.stone_count
    and ($artifacts.entries | map(select(.kind == "manifest-bin")) | length) == 1
    and ($artifacts.entries | map(select(.kind == "manifest-jsonc")) | length) == 1
    and ($artifacts.entries | map(.byte_count) | add) == $artifacts.total_bytes;

def valid_bundle_observation($point; $artifacts):
  exact_keys(["point", "artifact_count", "total_bytes", "ledger_sha256"])
  and .point == $point
  and .artifact_count == $artifacts.artifact_count
  and .total_bytes == $artifacts.total_bytes
  and .ledger_sha256 == $artifacts.ledger_sha256;

def valid_fixture($expected_name):
  . as $fixture
  | exact_keys([
      "name",
      "plans",
      "build_locks",
      "publications",
      "artifacts",
      "bundle_observations"
    ])
    and $fixture.name == $expected_name
    and ($fixture.plans | exact_keys(["first", "repeat"]))
    and ($fixture.plans.first | valid_plan_observation)
    and ($fixture.plans.repeat | valid_plan_observation)
    and $fixture.plans.first == $fixture.plans.repeat
    and ($fixture.build_locks | exact_keys(["first", "repeat"]))
    and ($fixture.build_locks.first | valid_build_lock_observation("written"))
    and ($fixture.build_locks.repeat | valid_build_lock_observation("unchanged"))
    and $fixture.build_locks.first.byte_count == $fixture.build_locks.repeat.byte_count
    and $fixture.build_locks.first.sha256 == $fixture.build_locks.repeat.sha256
    and $fixture.publications == {"first": "published", "repeat": "reused"}
    and ($fixture.publications | keys_unsorted == ["first", "repeat"])
    and ($fixture.artifacts | valid_artifacts($expected_name))
    and ($fixture.bundle_observations | type == "array")
    and ($fixture.bundle_observations | length) == 3
    and (
      $fixture.bundle_observations[0]
      | valid_bundle_observation("published-after-first"; $fixture.artifacts)
    )
    and (
      $fixture.bundle_observations[1]
      | valid_bundle_observation("staged-after-repeat"; $fixture.artifacts)
    )
    and (
      $fixture.bundle_observations[2]
      | valid_bundle_observation("published-after-repeat"; $fixture.artifacts)
    );

def fixture_names:
  [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "desktop-integration",
    "external-test-vectors",
    "factory-override",
    "font-family",
    "generated-config",
    "generated-shell",
    "gettext-localization",
    "go-module",
    "header-only-library",
    "hooks-patch",
    "meson",
    "multiple-sources",
    "pgo-workload",
    "plugin-output",
    "post-install-smoke-test",
    "python-module",
    "relation-policy",
    "split",
    "system-integration-assets",
    "userspace-profile"
  ];

length == 1
and (
  .[0] as $proof
  | fixture_names as $fixture_names
  | $proof
  | exact_keys([
    "schema",
    "git_commit",
    "git_tree",
    "selection",
    "required_execution",
    "bundle_ledger_schema",
    "totals",
    "fixtures",
    "result"
  ])
  and $proof.schema == "cast.fixtures-ci-proof.v2"
  and $proof.git_commit == $commit
  and $proof.git_tree == "clean"
  and $proof.selection == "all"
  and $proof.required_execution == true
  and $proof.bundle_ledger_schema == "cast.fixtures-ci.bundle.v1"
  and ($proof.totals | exact_keys([
      "fixture_count",
      "execution_count",
      "bundle_validation_count",
      "stone_count",
      "manifest_count",
      "artifact_count",
      "artifact_bytes"
    ]))
  and $proof.totals.fixture_count == 28
  and $proof.totals.execution_count == 56
  and $proof.totals.bundle_validation_count == 84
  and $proof.totals.stone_count == 134
  and $proof.totals.manifest_count == 56
  and $proof.totals.artifact_count == 190
  and ($proof.totals.artifact_bytes | bounded_positive_integer(4294967296))
  and ($proof.fixtures | type == "array")
  and ($proof.fixtures | map(.name)) == $fixture_names
  and (
    [range(0; ($fixture_names | length))]
    | all(. as $index | ($proof.fixtures[$index] | valid_fixture($fixture_names[$index])))
  )
  and ($proof.fixtures | map(.artifacts.stone_count) | add) == $proof.totals.stone_count
  and ($proof.fixtures | map(.artifacts.manifest_count) | add) == $proof.totals.manifest_count
  and ($proof.fixtures | map(.artifacts.artifact_count) | add) == $proof.totals.artifact_count
  and ($proof.fixtures | map(.artifacts.total_bytes) | add) == $proof.totals.artifact_bytes
  and $proof.result == "passed"
)
