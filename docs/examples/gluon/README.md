
# Gluon package examples

Machine-local boot inputs have three standalone examples:

| Example | What it demonstrates |
|---|---|
| [`boot-topology-aliases-esp.glu`](boot-topology-aliases-esp.glu) | One explicit ESP selector, including its canonical PARTUUID and authored mount point, used for both ESP and BOOT. |
| [`boot-topology-distinct-xbootldr.glu`](boot-topology-distinct-xbootldr.glu) | Explicit ESP and XBOOTLDR selectors with different canonical PARTUUIDs and authored mount points. |
| [`root-filesystem.glu`](root-filesystem.glu) | One explicit opaque root locator, from which Rust materializes exactly one `root=<value>` kernel token. |

The focused `make forge-active-reblit-boot-topology-intent-test` lane copies
both exact files to the fixed `etc/cast/boot-topology.glu` location beneath a
retained installation and evaluates them through the production restricted
loader. Both import `cast.boot_topology.v2`. Each `PartitionSelector` contains
a mandatory canonical PARTUUID and a mandatory bounded absolute lexical
`mount_point` whose exact authored bytes are retained without canonicalization.

Mount points are untrusted lexical lookup hints, not storage authority. They
have no defaults and do not enable discovery, fallback, or `canonicalize`.
The descriptor-retained coordinator must still prove the exact namespace
attachment, mountinfo entry, mounted device, matching sysfs PARTUUID, and the
topology relationship. Its selected mountinfo entry must report `vfat`, the
per-mount flags `rw`, `nosuid`, `nodev`, `noexec`, and `nosymfollow`, and a
writable (`rw`) superblock. Those facts are retained and revalidated as closed
mountinfo policy evidence. Every topology pass now composes it with a private
destination-descriptor `fstat`/`fstatfs` sandwich proving stable directory
identity and the Linux MSDOS magic family. Retained sysfs snapshots now include
bounded kernel device names and fixed-512-sector partition geometry, and a
sealed expectation binds the parent-disk facts to one freshly revalidated view.
A separate strict pure parser now authenticates two complete, exactly matching
GPT table passes and returns exact ESP/XBOOTLDR geometry plus a role-independent
table fingerprint without table bytes or read authority. It also retains the
selected partition number, logical-block size, and image length and exposes one
private same-deadline inter-pass checkpoint for a future live descriptor owner.
Exact `/dev` `devtmpfs` mountinfo policy is also available. Pure reconciliation
can reject disagreement between sealed sysfs/GPT facts and exact opening and
closing injected block-node observations, but it is intentionally not live
descriptor or read-provenance authority. The production retained block-device
binding, write authority, and durability remain open. A pure bounded
destination classifier can now distinguish stable absence, exact bytes, and
different bytes while
rejecting FAT aliases and namespace/content races, but its retained-descriptor
production observer is not wired yet. Its strict bounded raw `getdents64` parser
is implemented without syscalls; actual fresh directory descriptions and
descriptor-rooted reads remain open. The `nosymfollow` requirement gives
the future boot publisher an effective Linux 5.10-or-newer admission boundary
without changing the generic `linux_fs` Linux 5.6 compatibility baseline.
`cast.boot_topology.v1` cannot be
migrated automatically because it has no mount points; administrators must
rewrite it explicitly rather than let Cast guess from the host.

The complete capture and future publication boundary is specified in
[`active-reblit-mounted-boot-topology.md`](../../architecture/active-reblit-mounted-boot-topology.md).

The root-filesystem example is checked separately by
`make forge-active-reblit-root-filesystem-intent-test`. It imports only
`cast.root_filesystem.v1` and is installed as the mandatory machine-local
`/etc/cast/root-filesystem.glu` source. Its closed v1 value contains only
`{ root: String }`. Rust accepts one nonempty, bounded, whitespace-free
printable-ASCII locator and owns the single `root=` prefix. The locator is
authenticated authored intent, not evidence that a device or filesystem
exists. It is not inferred from boot topology, the running kernel, fstab,
udev, package command lines, or legacy disk probes. Rendering and publication
remain separated: the lifetime-bound aggregate and pure renderer are wired,
while the durable publisher is not. Before rendering, the aggregate reserves
the `root` key, rejects any package or local command-line duplicate, and emits
the authenticated root token exactly once per kernel.

The recipes under [`packages`](packages) exercise the public
`cast.package.v3` interface as ordinary, pure Gluon programs. They are
deliberately small enough to study, but together cover the package shapes
needed by a declarative userspace.

| Example | What it demonstrates |
|---|---|
| [`minimal`](packages/minimal/stone.glu) | A source-less package using only versioned defaults. |
| [`source-less-generated-config`](packages/source-less-generated-config/stone.glu) | A deterministic configuration artifact generated from authored data without an upstream source. |
| [`binary-release`](packages/binary-release/stone.glu) | An architecture-specific prebuilt archive with an explicit install contract. |
| [`platform-binary-factory`](packages/platform-binary-factory/stone.glu) | An explicit platform record selecting a prebuilt archive, target, and runtime loader without host discovery. |
| [`raw-script-package`](packages/raw-script-package/stone.glu) | A single locked, renamed, non-archive source installed with an explicit interpreter relation. |
| [`cmake`](packages/cmake/stone.glu) | CMake flags, checks, and typed build dependencies. |
| [`meson`](packages/meson/stone.glu) | Meson configuration and pkg-config dependencies. |
| [`cargo`](packages/cargo/stone.glu) | An offline Cargo build with features and explicit binaries. |
| [`go-module`](packages/go-module/stone.glu) | An offline vendored Go module with disabled network resolution, isolated caches, tests, and split documentation. |
| [`zig-project`](packages/zig-project/stone.glu) | A vendored Zig project with phase-local caches, tests, and runtime/development output relations. |
| [`python-module`](packages/python-module/stone.glu) | An offline Python wheel build with separate runtime executable, module, and test dependencies. |
| [`autotools`](packages/autotools/stone.glu) | Autotools flags, tests, and architecture selection. |
| [`desktop-application`](packages/desktop-application/stone.glu) | Desktop metadata, activation assets, runtime relations, and validation hooks. |
| [`font-family`](packages/font-family/stone.glu) | A data-only package with explicit font and documentation outputs. |
| [`header-only-library`](packages/header-only-library/stone.glu) | A compile-checked header-only library with development metadata and no runtime-library edge. |
| [`gettext-catalogs`](packages/gettext-catalogs/stone.glu) | Architecture-independent message catalogs compiled and checked by an explicit native tool closure. |
| [`firmware-bundle`](packages/firmware-bundle/stone.glu) | Architecture-independent firmware, device metadata, and license data installed without compilation. |
| [`conditionals`](packages/conditionals/stone.glu) | A pure package function driven by typed feature values. |
| [`optional-component-source-graph`](packages/optional-component-source-graph/stone.glu) | One typed feature adding a locked source, native tool, setup hook, and output as a coherent graph. |
| [`backend-choice-factory`](packages/backend-choice-factory/stone.glu) | A closed Gluon variant selecting one mutually exclusive build and runtime backend. |
| [`release-source-factory`](packages/release-source-factory/stone.glu) | One explicit release record driving package metadata, source identity, and materialization names. |
| [`service-family-factory`](packages/service-family-factory/stone.glu) | One release and a closed member selector producing an exact daemon, client, or integration closure. |
| [`release-override`](packages/release-override/stone.glu) | An explicit attribute patch replacing package metadata and the complete source list together. |
| [`factory-override`](packages/factory-override/stone.glu) | Dependency-argument overrides followed by a typed attribute patch. |
| [`explicit-package-scope`](packages/explicit-package-scope/stone.glu) | Multiple explicit factories receiving one authored capability scope without reflection or recursive package-set magic. |
| [`explicit-package-set-extension`](packages/explicit-package-set-extension/stone.glu) | A non-recursive package-set extension passed explicitly into a source-less userspace bundle. |
| [`platform-factory`](packages/platform-factory/stone.glu) | A pure factory receiving explicit platform policy and dependency capabilities from local modules. |
| [`kernel-module-factory`](packages/kernel-module-factory/stone.glu) | A kernel-specialized package factory with an exact headers output, ABI release, module path, and target set. |
| [`layered-overrides`](packages/layered-overrides/stone.glu) | Ordered total package transformations with visible prepend, append, and scalar replacement semantics. |
| [`locked-template-substitution`](packages/locked-template-substitution/stone.glu) | A locked template specialized by one explicit Gluon values record through typed `Run` steps and exact tool providers. |
| [`dependency-roles`](packages/dependency-roles/stone.glu) | Native, target, check, runtime, output, binary, library, and interpreter relations. |
| [`shared-capability-origins`](packages/shared-capability-origins/stone.glu) | One provider request retaining every distinct package, executable, and output origin in the frozen closure. |
| [`custom-steps`](packages/custom-steps/stone.glu) | Explicit `Run` and declared-program `Shell` steps. |
| [`manual-compiler-pipeline`](packages/manual-compiler-pipeline/stone.glu) | A fully explicit preprocess, compile, link, check, and install pipeline with split outputs. |
| [`hooks`](packages/hooks/stone.glu) | Structural pre/post hooks around a standard builder. |
| [`post-install-smoke-test`](packages/post-install-smoke-test/stone.glu) | An installCheck-style hook executing the artifact from the staged install tree. |
| [`multiple-sources`](packages/multiple-sources/stone.glu) | Archives, locked Git, renamed files, unpack policy, and destinations. |
| [`explicit-git-subprojects`](packages/explicit-git-subprojects/stone.glu) | Three independently locked Git trees composed into one explicit subproject layout without recursive fetching. |
| [`native-codegen-target-library`](packages/native-codegen-target-library/stone.glu) | A build-platform generator and target-platform library kept in distinct typed dependency roles. |
| [`split-outputs`](packages/split-outputs/stone.glu) | Runtime, development, documentation, and root output rules. |
| [`typed-output-routing`](packages/typed-output-routing/stone.glu) | Ordered catch-all, executable, symlink, and special-file collection rules across explicit outputs. |
| [`userspace-role-factory`](packages/userspace-role-factory/stone.glu) | Closed workstation, server, and builder roles selected through an ordinary pure function rather than module merging. |
| [`variant-matrix-factory`](packages/variant-matrix-factory/stone.glu) | Two exhaustive typed axes forming one dependency and build-policy matrix without independent boolean drift. |
| [`conflicts`](packages/conflicts/stone.glu) | Typed conflicts, exclusions, provides, and path kinds. |
| [`options-tuning`](packages/options-tuning/stone.glu) | Toolchain choice, hardening, LTO, optimization, and package switches. |
| [`profiles-emul32`](packages/profiles-emul32/stone.glu) | Profile-specific builders and 32-bit dependency roles. |
| [`target-profile-specialization`](packages/target-profile-specialization/stone.glu) | Exact target-name selection replacing a package's builder, hooks, and dependency roles as one profile. |
| [`meta-package`](packages/meta-package/stone.glu) | A source-less package that declaratively composes a userspace. |
| [`output-policy-factory`](packages/output-policy-factory/stone.glu) | A higher-order package factory whose selected feature policy drives both the build and a typed multi-output graph. |
| [`output-tool-wrapper`](packages/output-tool-wrapper/stone.glu) | A wrapper generated by a program from a named dependency output. |
| [`generated-schema-library`](packages/generated-schema-library/stone.glu) | C sources generated and compiled by a tool bound to one named dependency output. |
| [`patch-series`](packages/patch-series/stone.glu) | An ordered typed patch series factored into a local Gluon module. |
| [`external-patch-source`](packages/external-patch-source/stone.glu) | A separately locked raw patch applied to one structurally extracted primary source. |
| [`external-test-vectors`](packages/external-test-vectors/stone.glu) | A CMake check graph that explicitly admits a separately locked raw corpus through a declared copy capability. |
| [`pgo-workload`](packages/pgo-workload/stone.glu) | A structural offline training workload with multi-stage profile-guided optimization. |
| [`nodejs-vendored-application`](packages/nodejs-vendored-application/stone.glu) | A Node.js application built against a fully materialized local dependency tree without npm or registry access. |
| [`maven-application`](packages/maven-application/stone.glu) | A Java application built from an admitted Maven repository in strict offline mode. |
| [`realistic-daemon`](packages/realistic-daemon/stone.glu) | A larger daemon with hooks, services, multiple outputs, dependencies, and tuning. |
| [`system-integration-assets`](packages/system-integration-assets/stone.glu) | A service unit, sysusers, tmpfiles, udev, and polkit policy shipped as one declarative integration package. |

Run the complete checked-in proof lane from the repository root:

```sh
make examples
```

That target:

1. discovers every package directory and runs public `cast recipe check` and
   `cast recipe eval`, requiring deterministic repeated evaluation and no
   source-tree mutation;
2. freezes every example with hermetic local source and repository fixtures,
   writes and reuses its exact `build.lock.glu`, and requires identical plan
   bytes and derivation IDs; and
3. proves that the synthetic metadata-only providers used for planning cannot
   cross the frozen executable boundary or publish a derivation.

The example URLs intentionally use `example.invalid`; `make examples` never
depends on those remote endpoints. The planner proof substitutes
content-addressed local fixtures. This lane deliberately does not claim that
the fictional upstream projects can be built. Real compilation and packaging
belong to the contentful execution-fixture lane below.

## Representative execution fixtures

Twenty-six separate fixtures cover representative package shapes. Twenty-three contain
small, real source trees for Autotools, configured Autotools with an
intentionally disabled check phase, Cargo, feature-selected multi-binary Cargo,
vendored/offline Cargo, CMake, custom-step, pre-setup patch hooks, Meson,
generated daemon assets, Gluon factory/override composition, a runtime-loaded
plugin with an explicit output relation, a staged post-install smoke test, and
an independently compiled staged header-only interface, native split-output
builds, one mixed archive, exact-commit Git, and raw-file build, compiled
gettext localization, declarative system-integration assets, declarative
desktop integration, a deterministic font family, and a pinned vendored Go
module. The
patch-hook case now binds two independent sources: a deterministic XZ USTAR
archive and a raw HTTPS-identified patch. Only the archive is extracted; the
declared pre-setup patch program consumes the separately materialized bytes.
The primary Autotools case carries only authored `configure.ac`, `Makefile.am`,
and C input. Its declared native `binary(autoreconf)` provider regenerates the
build system in pre-setup before the structural builder supplies its frozen
build/host triples, runs the generated test suite, and installs the result.
The CMake case declares `cmake(zlib)` as a target build input, resolves the
exact pinned `zlib-devel` provider, and performs a real `compress2`/`uncompress`
round trip under CTest. Its bundle checks bind the manifest BuildDepends entry
to that provider and require the installed ELF and emitted Stone metadata to
carry `soname(libz.so.1(x86_64))`.
The `external-test-vectors` CMake fixture keeps its executable source and its
conformance corpus as two independently locked inputs. The primary USTAR is
the only extracted source; a typed pre-check Bash hook uses only its declared
`cp` capability to admit the raw JSON into private build scratch before CTest.
The frozen plan pins both source identities and proves that source order 1 is
never interpreted as an archive. Its one-output bundle may contain only the
installed frame-codec ELF: the corpus is check-only and must not enter the
Stone or create a runtime relation. The supplemental hostile-host lane proves
the check fails without the corpus, succeeds with the exact bytes, and rejects
a mutated vector, but it is not delegated Stone execution evidence.
The second CMake fixture turns the documented `post_install` pattern into an
offline executable proof. Its ordinary check phase runs the build-tree target,
then its post-install hook invokes only
`${CAST_INSTALL_ROOT}${CAST_BINDIR}/staged-probe`. The probe rejects an
invocation path that does not exactly match that staged location. Only after
that check does it write the fixed
`/usr/share/cast/post-install-smoke-test.proof` bytes; package decoding requires
that proof artifact as well as the tracked installed ELF behavior.
The header-only fixture installs its interface under a path which does not
exist in the source tree, then invokes the pinned compiler with `-nostdinc`
against a consumer containing fixture-specific compile-time assertions. Its
two decoded Stone outputs prove that only license metadata enters `out`, while
the exact header and pkg-config bytes enter a dependency-free `devel` output.
The Meson case exercises the complementary dependency roles. It resolves the
same pinned library through `pkgconfig(zlib)`, compiles and links a real zlib
round trip, and admits `binary(file)` only as a check input. A non-installed
native checker invokes that capability during `meson test` and verifies the
built PIE executable; the packaged program retains libz but cannot acquire a
runtime `file` relation. The exact build and check origins, provider IDs,
transitive closure, manifest entries, installed ELF, and emitted Stone metadata
are all frozen independently.
The `gettext-localization` fixture compiles deterministic French and German GNU
message catalogs, then builds a temporary libc consumer and requires both
translations to execute without falling back to the source text. Only the two
catalogs and their license enter its single `out`; `msgfmt`, the compiler, the
consumer, and every other build tool remain outside the package and contribute
no runtime relation. Its supplemental host lane repeats catalog compilation and
translation checks, including missing-catalog fallback rejection, but is not a
Stone/container run and proves neither locale deployment nor host activation.
The `system-integration-assets` fixture turns the declarative integration
example into an install-only package with real bytes. Its one explicit `out`
routes exactly eight files: a staged helper, a systemd unit, sysusers and
tmpfiles declarations, a udev rule, a conservative polkit rule, its matching
action XML, and a license. The frozen closure binds every declared build,
check, and runtime capability to exact dash, install, systemd, udev, polkit,
and xmllint providers. The offline contract pins source/archive identity,
install and check scripts, and the polkit rule bytes. A supplemental lane
copies the authored files into a disposable staged root, self-tests the helper,
and runs the declared systemd, sysusers, tmpfiles, udev, and XML checks there;
it does not install into host paths. The delegated bundle golden requires all
eight exact bytes and modes if a supported execution host emits the Stone.
These checks do not enable or start a service, create a host account, apply
tmpfiles to host state, trigger a device event, load polkitd, authorize an
action, boot, reboot, or prove a transaction or rollback.
The `desktop-integration` fixture installs one helper plus a desktop entry,
AppStream metadata, GSettings schema, shared-MIME declaration, scalable icon,
and license. Its exact closure contains 99 packages. The check phase binds
`desktop-file-validate`, `glib-compile-schemas`, `appstreamcli`,
`update-mime-database`, and `xmllint` to pinned providers. Cache generation is
tested only in disposable build scratch space: `gschemas.compiled`,
`mime.cache`, `mimeinfo.cache`, and `icon-theme.cache` are forbidden from the
immutable output so transaction triggers can generate them for the deployed
package set. A hostile-environment host lane validates the same staged files
without touching the host desktop, MIME, schema, or icon databases. It is
supplemental validation, not a GUI, activation, transaction, or rollback test.
The `font-family` fixture installs a self-authored Regular/Bold TrueType family
from a deterministic 30,720-byte archive with SHA-256
`8710f0728fbde240fd94ce8bce46c4e4d71336b8470416e8da7c0895dc2d700c`.
Its check phase binds `fc-scan` to the pinned fontconfig provider and verifies
the exact family, style, format, full name, and PostScript name. Its one `out`
contains exactly both TTFs and `OFL.txt`, all mode `0644`; the Rust generator,
provenance, and every generated font cache remain outside the Stone. The exact
closure is 63 packages and 213,892,544 bytes with no runtime relation. The
supplemental hostile-host scan proves deterministic bytes and metadata, not
font-cache activation, graphical rendering, deployment, or rollback.
The `go-module` fixture builds a real command from a self-authored module and
its authentic checked-in vendor tree. Its typed custom builder disables the
network, module proxy, checksum database, workspace discovery, toolchain
downloads, CGO, and ambient Go configuration; it selects the frozen local
toolchain and requires `-mod=vendor`. Both Go tests and the installed command's
self-test run before publication. The resulting one-output Stone contains the
exact static ELF plus its MIT license, carries no runtime dependency, and must
not retain module sources or vendor contents. Its exact 71-package closure is
the userspace baseline plus the single pinned Go compiler package. The
supplemental hostile-host lane proves two clean offline builds are
byte-identical and that deleting the vendor tree fails; it is not delegated
Stone execution evidence.
The `python-module` fixture builds a real `py3-none-any` wheel from a
self-authored `pyproject.toml` source tree through the PEP 517 setuptools
backend. Build, installer, setuptools, pytest, interpreter, and
typing-extension roles resolve from one exact 76-package, 214,660,406-byte
closure. The recipe builds without isolation or network access, runs the test
suite and module self-test, stages installation with the pinned installer, and
declares only Python plus `typing-extensions` at runtime. Its one-output bundle
contract pins the console script, module, metadata, license, entry points, and
RECORD. The hostile-host lane rebuilds two byte-identical wheels, executes the
extracted wheel module, and proves a missing module fails; it remains supplemental
and does not claim a delegated Stone build.
The other three fixtures are deliberately source-less.
`generated-config` authors deterministic configuration bytes
and installs them with only its frozen `bash` and `install` providers. It has no
source lock, archive, network access, host shim, or mounted recipe input.
`generated-shell` authors and executes a complete shell application through its
frozen Bash provider, then installs the exact script with an explicit runtime
interpreter relation. It likewise has no source lock, archive, network access,
host shim, or mounted recipe input.
`userspace-profile` goes further: it has no build tools and all five authored
phases are empty. Its one empty `out` package carries only the exact runtime
package relations `bash`, `uutils-coreutils`, `findutils`, `ca-certificates`,
and `xz`. Run the proof lanes from the repository root:

The checked-in source matrix has twenty-three deterministic tar streams: eighteen
plain USTAR archives, vendored Cargo as deterministic gzip, two deterministic
XZ archives, and the generated-daemon and Go fixtures as deterministic
Zstandard. It
also contains three independently locked raw files and one deterministic Git
bundle.
`make fixture-sources` rebuilds all twenty-six archive/raw artifacts plus that one
bundle; the offline lane rejects any format, filename, order, unpack policy,
commit, normalized Git tree, or digest drift. The source generator fixes Git's
identity, timestamps, refs, and configuration before producing the bundle.
The default `flake.nix` development shell supplies gzip, XZ, Zstandard, and Git
for this hermetic generation path.

```sh
make execution-fixtures
make delegated-execution-preflight
make bootstrap-fixtures
make bootstrap-fixtures FIXTURE=cmake
make bootstrap-fixtures FIXTURE=desktop-integration
make bootstrap-fixtures FIXTURE=external-test-vectors
make bootstrap-fixtures FIXTURE=font-family
make bootstrap-fixtures FIXTURE=gettext-localization
make bootstrap-fixtures FIXTURE=go-module
make bootstrap-fixtures FIXTURE=multiple-sources
make bootstrap-fixtures FIXTURE=python-module
make bootstrap-fixtures FIXTURE=system-integration-assets
make delegated-execution-fixtures FIXTURE=cmake
make external-test-vectors-fixture-test
make font-family-fixture-test
make go-module-fixture-test
make python-module-fixture-test
make fixtures-ci
```

`make execution-fixtures` is the offline lane: it byte-checks the deterministic
source artifacts, validates the pinned Stone index and closure declaration, and
proves that each recipe resolves to its own exact, sorted package-ID closure
and that their union is the exact 172-package, 383,747,528-byte aggregate
bootstrap closure. `make
bootstrap-fixtures` fetches and verifies any missing pinned Stone files,
materializes the production-format root mirror, then attempts to build,
package, and reproduce every fixture. Set `FIXTURE=<name>` to select exactly
one of `autotools`, `autotools-options`, `cargo`, `cargo-features`,
`cargo-vendored`, `cmake`, `custom`, `daemon-generated`, `desktop-integration`,
`external-test-vectors`, `factory-override`, `font-family`,
`generated-config`, `generated-shell`, `gettext-localization`, `go-module`, `header-only-library`, `hooks-patch`,
`meson`, `multiple-sources`, `plugin-output`,
`post-install-smoke-test`, `python-module`, `split`, `system-integration-assets`, or
`userspace-profile`;
`FIXTURE=all` is the default, and any other value is rejected before execution.
The selector also
works with `make bootstrap-fixtures-offline` when the package store has already
been prepared. Execution may skip when the host
cannot create the required namespaces; pass `REQUIRE_EXECUTION=1` to reject
that skip. A skipped developer run is not evidence that contentful execution or
bundle reproduction succeeded. `make fixtures-ci` ignores developer fixture
selection, runs all twenty-six, and always requires execution.

`make delegated-execution-preflight` is the required-only, pre-download host
gate. It builds the harness-free probe but neither fetches nor reads the Stone
bootstrap closure, then exercises the same delegated service, `clone3`, cgroup,
ID-map, credential, mount, and pinned-bind boundary used by real execution.
CI runs it before restoring the bootstrap cache, so an incapable host fails
quickly rather than downloading packages it cannot execute.

Every selected fixture, including all three source-less fixtures, goes through
the same real execution, Stone package decoding, manifest decoding, and locked
reproduction path. The generated configuration golden freezes its exact
`/usr/share/cast/generated-config.conf` bytes, `0644` mode, package metadata,
relations, and manifest membership. Replanning must reuse the unchanged build
lock, and the repeated build must reproduce every emitted `.stone` and manifest
byte-for-byte. The generated shell golden fixes the script bytes, executable
mode, Bash relation, and command provider. The plugin golden pins both compiler
commands and proves that the host uses the dynamic-loader API, validates the
plugin identity, and depends explicitly on the `plugins` output rather than an
accidental ELF link. Its native-ELF checks require PIE, RELRO, immediate
binding, a non-executable stack, separated writable/executable loads, no runtime
search path or text relocations, and exact build-ID debug payloads. The
multiple-sources golden separately pins its XZ application archive, exact Git
commit and normalized exported tree, and raw schema bytes. The raw schema stays
`unpack = false`; one typed pre-setup Bash step, with only its declared `cp`
capability, copies it from `CAST_SOURCE_DIR` into the private application tree
without overwriting an existing file and while preserving mode and timestamp.
The build then consumes all three identities in one exact executable output. The
userspace-profile golden additionally decodes its
production-format `.stone` to prove a Meta-only payload topology, no layout or
content bytes, and exactly the five frozen runtime relations. An
optional-capability `SKIP` remains explicitly non-success.

The required all-fixture lane publishes one bounded v2 JSON receipt only after
all twenty-six fixtures complete both executions. It records exactly 52
executions, 78 bundle validations, 131 Stones, 52 manifests, and 183 artifacts,
plus repeated plan and lock identities, actual publication outcomes, the
sorted Stone/manifest inventory, and three matching bundle-ledger observations
per fixture. Mason derives those ledgers from the authenticated raw bundle
bytes and publishes the receipt without replacing an existing file. The exact
shell validator rejects duplicate or reordered keys, unexpected fields,
unbounded values, inconsistent totals, and ledger framing that does not match
the recorded names, sizes, and raw-byte digests. `make fixture-proof-test`
exercises the Rust producer, the adversarial validator, and their direct
cross-boundary contract without claiming that an offline test performed the
live delegated builds.

That receipt is a deterministic CI result, not a signed remote attestation.
Its trust root is the sealed Mason execution path: post-run validation binds
the ledger to Mason's recorded raw-byte digests but cannot reread artifacts
which were not retained beside the receipt. A capability skip, a hand-authored
JSON document, or a receipt from a different commit therefore does not prove
fixture execution.

The execution stage does not run under Rust's multithreaded test harness. Its
runner first builds Mason's feature-gated `harness = false` test target outside
the delegated unit, selects the one exact Cargo-reported test executable with
`jq`, and only then starts that executable as the transient delegated service.
`make delegated-execution-fixtures` runs this stage directly against an
already prepared package store. Cast remains the workspace's sole binary
target.

Descriptor-safe filesystem and state operations retain a Linux 5.6 baseline
because they use `openat2(2)`. Full frozen execution is currently limited to
Linux x86_64 and requires Linux 5.14 or newer: its fail-closed boundary uses
`CLONE_INTO_CGROUP` (5.7), `CLOSE_RANGE_CLOEXEC` (5.11), `mount_setattr(2)`
(5.12), and the mandatory race-safe `cgroup.kill` interface (5.14). It also
requires user and mount namespaces plus a systemd cgroup-v2 unit configured
with `Delegate=cpu memory pids` and
`DelegateSubgroup=cast-supervisor`. The fixture runner creates this transient
`Type=exec` service with cgroup-lifetime exit and control-group cleanup
semantics. Each invocation owns a random, authenticated unit name, stops that
unit on interruption, and imposes a two-hour runtime plus a thirty-second
stop deadline; a leaked descendant therefore cannot keep the Make invocation
alive forever. A reachable systemd user manager is optional only for the
ordinary developer lane and is mandatory when `REQUIRE_EXECUTION=1`. Cast
itself does not synthesize or migrate into a delegation:
`/proc/self/cgroup` must already contain exactly one unified entry ending in
`/cast-supervisor`, or execution fails before the container child is created.
For an unprivileged caller, the current mapper specifically requires
`/usr/bin/newgidmap` and at least one delegated GID in `/etc/subgid`; the usual
`uidmap` package provides the helper. The UID map is written directly, so
`/usr/bin/newuidmap` and `/etc/subuid` are not currently consumed. Do not use
`unshare --user --map-root-user --mount true` as proof that this boundary is
available: that convenience mapping can permanently set `setgroups=deny` and
therefore avoid the production requirement to clear inherited supplementary
groups. The harness-free delegated runner instead performs a small
production-policy `clone3`/cgroup/container activation inside the same
transient service, before it reads the bootstrap index or materializes the
package root. An optional denial stops there with an explicit `SKIP`;
`REQUIRE_EXECUTION=1` fails there without downgrading to a skip.

Some hosts disable unprivileged namespaces through
`kernel.unprivileged_userns_clone`. Ubuntu hosts may additionally set
`kernel.apparmor_restrict_unprivileged_userns=1`; the required CI lane enables
the former and temporarily sets the latter to `0`. Changing either setting is
a host security-policy decision and may require an administrator.
