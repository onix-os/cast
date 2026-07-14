<!--
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Package authoring

Cast packages are pure Gluon programs evaluated through
`cast.package.v3`. A recipe may import local modules and call functions, but
Rust receives one concrete, validated `PackageSpec`; it never retains a Gluon
closure. The retired recipe-v1 ABI is not a compatibility path.

This guide describes the current authoring, planning, and execution contract.
`cast recipe plan`, `cast recipe explain`, and normal `cast build` all use the same
target-specific frozen derivation model.

## A package factory and explicit scope

Put reusable package construction in a function. Its argument is an ordinary
record containing symbolic dependencies selected by the caller:

```gluon
// package.glu
let b = import! cast.package.v3
let cmake = import! cast.builders.cmake.v2

\scope ->
    let base = b.mk_package (b.meta {
        pname = "hello",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/hello",
        license = ["MPL-2.0"],
    })
    {
        builder = cmake.builder {
            flags = ["-DBUILD_TESTS=ON"],
            .. cmake.defaults
        },
        native_build_inputs = [scope.pkgconf],
        build_inputs = [scope.zlib],
        sources = [
            b.source.archive
                "https://example.invalid/hello-1.0.0.tar.xz"
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ],
        .. base
    }
```

The root `stone.glu` supplies the scope and therefore produces the concrete
package value:

```gluon
let b = import! cast.package.v3
let make = import! "./package.glu"

make {
    pkgconf = b.dep.binary "pkgconf",
    zlib = b.dep.pkgconfig "zlib",
}
```

`meta.pname` is also an artifact filename component. It must be non-empty and
use only ASCII letters, digits, `+`, `-`, `.`, or `_`; `.` and `..` are not
package names. `meta.version` must begin with a digit and be one normalized
filename component: path separators, traversal components, and control
characters are rejected. Cast checks these rules during package evaluation,
again when validating the frozen plan, and before creating recipe-keyed host
paths.

There is no `callPackage`-style argument-name reflection. A missing scope field
is a Gluon type error. Scope values remain symbolic during evaluation;
provider resolution occurs later when Cast creates `build.lock.glu`.
Scopes remain ordinary, nonrecursive imported Gluon records; there is no
second Rust `PackageSet` ABI or hidden alias-provenance layer. Local output
cycles are rejected during package validation, and cycles in the resolved
package closure report the concrete dependency path.

## Dependency roles

Use typed dependency constructors rather than provider strings:

```gluon
b.dep.package "zlib"
b.dep.output (b.package_ref "llvm") "clang"
b.dep.binary "cmake"
b.dep.system_binary "sh"
b.dep.pkgconfig "openssl"
b.dep.pkgconfig32 "zlib"
b.dep.soname "libz.so.1"
b.dep.cmake "Qt6"
b.dep.python "setuptools"
b.dep.interpreter "/usr/bin/python3"
```

Put them in the field matching their purpose:

- `native_build_inputs`: programs executed while building;
- `build_inputs`: target libraries, headers, or other build-time inputs;
- `check_inputs`: inputs used only by checks;
- `outputs[*].runtime_inputs`: runtime relations of one emitted output.

The shared `stone::relation` model is the canonical parser and representation
used by Cast and package conversion. Local output references are
validated before planning, including missing outputs and cycles.

## Standard builders

Import one standard builder module and start from its `defaults` record when
overriding typed settings:

| Module | Settings | Structural phases | Environment marker |
|---|---|---|---|
| `cast.builders.cmake.v2` | `flags`, `run_tests` | configure, build, install, test | CMake |
| `cast.builders.meson.v2` | `flags`, `run_tests` | setup, build, install, test | Meson |
| `cast.builders.cargo.v2` | `features`, `binaries`, `run_tests` | build, install, test | Cargo |
| `cast.builders.autotools.v2` | `flags`, `run_tests` | configure, build, install, test | Autotools |

Booleans use `b.boolean.true` and `b.boolean.false`. For example:

```gluon
let b = import! cast.package.v3
let cargo = import! cast.builders.cargo.v2

cargo.builder {
    features = ["cli", "tls"],
    binaries = ["hello", "helloctl"],
    run_tests = b.boolean.true,
}
```

Each module returns one concrete `BuilderSpec`: symbolic required-tool
capabilities, an environment marker, the ordered typed phase graph, and its
supported hook surface. Repository policy owns only the command templates for
those typed steps and the bindings selected by the marker. Rust lowers the two
records together; it does not invent phases or a second tool list. No builder
authors or lowers through `%cmake`, `%meson`, `%cargo`, `%configure`, or `%make`
action strings.

## Typed phases and hooks

The standard builder owns its phase body and declares which hook positions it
supports. Package or profile hooks add explicit steps before or after that
body:

```gluon
hooks = b.hooks {
    pre_build = [b.step.run (b.program.binary "generate-sources") []],
    post_install = [
        b.step.shell_with {
            interpreter = b.program.binary "bash",
            declared_programs = [b.program.binary "ln"],
            script = r#"ln -s hello "${CAST_INSTALL_ROOT}${CAST_BINDIR}/hi""#,
        },
    ],
    .. b.defaults.hooks
}
```

The current standard modules support `pre_` and `post_` positions for `setup`,
`build`, `check`, `install`, and `workload`; unsupported populated hooks are a
package-validation error. Hook and builder step order is preserved, but every
frozen shell step runs in its own declared interpreter process. `b.step.shell`
is shorthand for a Gluon-authored `/usr/bin/bash` capability with no additional
programs. Filesystem effects persist;
process-local variables and working-directory changes never cross step
boundaries. Put a one-command environment assignment directly on the command
which consumes it.

The package ABI exposes the typed standard-step constructors used by the
embedded modules, plus `b.step.run`, `b.step.shell`, and `b.step.shell_with`
for explicit custom work. It deliberately has no `cargo_fetch` step: a frozen
build cannot resolve or download Cargo dependencies. Cargo inputs must already
be present in a locked source, with Cargo configured to use that vendored tree;
the standard Cargo builder runs with `--frozen`. Package authors normally
select a standard module rather than rebuilding its phase graph step by step.

## Custom shell builders

Use a custom builder only when the build cannot be represented by a standard
builder. Every phase is an explicit `b.phase`. A direct `Run` binds one program
to the dependency capability which provides it; a `Shell` binds its interpreter
and every non-builtin program invoked by the script:

```gluon
let b = import! cast.package.v3

let scripts = b.scripts {
    setup = b.phase [b.step.run (b.program.binary "zig") ["build", "--fetch"]],
    build = b.phase [b.step.run (b.program.binary "zig") ["build", "-Doptimize=ReleaseSafe"]],
    check = b.phase [b.step.run (b.program.binary "zig") ["build", "test"]],
    install = b.phase [
        b.step.shell_with {
            interpreter = b.program.binary "bash",
            declared_programs = [b.program.binary "zig"],
            script = r#"zig build install --prefix "${CAST_INSTALL_ROOT}${CAST_PREFIX}""#,
        },
    ],
    .. b.defaults.scripts
}

{
    builder = b.builder.custom scripts [],
    .. b.mk_package (b.meta {
        pname = "zig-hello",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/zig-hello",
        license = ["MIT"],
    })
}
```

`Shell` is an explicit, literal escape hatch. It never enters the former macro
parser: `%name` and `%(name)` have no special meaning. `b.program.binary` and
`b.program.system_binary` construct canonical `/usr/bin` and `/usr/sbin`
bindings. `b.program.package` and `b.program.output` bind an arbitrary
normalized absolute guest path to a package or output capability. Relative,
traversing, mismatched, and non-executable relation bindings are rejected
during package validation.

Every step receives a frozen build context. Shell steps access it through
stable variables including:

- `CAST_PACKAGE_NAME`, `CAST_PACKAGE_VERSION`, and `CAST_PACKAGE_RELEASE`;
- `CAST_SOURCE_DIR`, `CAST_BUILD_ROOT`, `CAST_WORK_DIR`,
  `CAST_INSTALL_ROOT`, and `CAST_BUILDER_DIR`;
- `CAST_PREFIX`, `CAST_BINDIR`, `CAST_LIBDIR`,
  `CAST_LIBEXECDIR`, `CAST_DATADIR`, and `CAST_VENDORDIR`;
- `CAST_JOBS` and `CAST_PGO_DIR`.

These values are resolved before execution and are part of the canonical
derivation plan. They are not read from the host process environment.

## Outputs

Every package must contain exactly one output named `out`. Additional output
names are local names; Cast currently lowers `dev` to `<pname>-dev` at the
internal packaging boundary.

`b.mk_package` starts with the deterministic output set exported by
`cast.package.v3` (root, documentation, development, debug, libraries,
32-bit, and demos). These are versioned package-ABI defaults: they are ordinary
Gluon values present in the concrete evaluated `PackageSpec`, not a hidden Rust
merge or a repository policy layer. A package can replace `outputs` explicitly
as below, and an incompatible change to the default set requires a new package
ABI version.

```gluon
let root = {
    summary = b.optional.set "Hello executable",
    paths = [b.path.exe "/usr/bin/hello"],
    .. b.output "out"
}

let development = {
    summary = b.optional.set "Hello development files",
    runtime_inputs = [b.dep.output (b.package_ref "hello") "out"],
    paths = [
        b.path.any "/usr/include/hello",
        b.path.symlink "/usr/lib/libhello.so",
    ],
    .. b.output "dev"
}

{
    outputs = [root, development],
    .. base
}
```

Path constructors are `any`, `exe`, `symlink`, and `special`. Outputs can also
set `description`, `provides_exclude`, `runtime_exclude`, and typed `conflicts`.

## Package patches and argument overrides

Change a factory argument before construction by updating its scope:

```gluon
let package = make {
    tls = b.dep.pkgconfig "libressl",
    .. packages
}
```

Change a completed package through the total typed patch algebra:

```gluon
let patch = b.package_patch {
    architectures = b.patch.array.replace ["x86_64"],
    build_inputs = b.patch.array.append [b.dep.package "extra-input"],
    outputs = b.patch.array.append [development],
    .. b.defaults.package_patch
}

b.override_attrs patch package
```

Scalar or record fields use `b.patch.keep` and `b.patch.set`. Arrays use
`keep`, `replace`, `prepend`, or `append`; replacing an array with `[]` is
different from keeping its existing value.

## Options, profiles, and tuning

`b.options` configures the toolchain and build behavior. Start from
`b.defaults.options` and update only intentional fields. Toolchains are
`b.toolchain.llvm` and `b.toolchain.gnu`; Boolean fields use the explicit
Boolean constructors.

Target-specific profiles use `b.profile "name"` or `b.profile_with`. A profile
selects its own builder, hooks, and native/build/check inputs. Tuning entries
use `b.named` with `b.tuning.enable`, `disable`, or `config`.

## Source resolution and `sources.lock.glu`

Declare archives or Git requests in `sources`:

```gluon
sources = [
    b.source.archive
        "https://example.invalid/hello.tar.xz"
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    b.source.git "https://example.invalid/hello.git" "v1.0.0",
]
```

Archive hashes are exactly 64 lowercase hexadecimal characters. They bind the
authored request, generated source lock, frozen derivation, and fetched bytes
to one canonical SHA-256 identity.

Use `archive_with` to set `rename`, `strip_dirs`, `unpack`, or `unpack_dir`, and
`git_with` to set `clone_dir`:

```gluon
b.source.git_with {
    url = "https://example.invalid/hello.git",
    git_ref = "v1.0.0",
    clone_dir = b.optional.set "hello-source",
}
```

`rename` and `clone_dir` must each be one safe filename component. `unpack_dir`
must be a normalized relative path without empty, `.` or `..` components;
`strip_dirs` and `unpack_dir` are rejected when `unpack` is false because they
would otherwise be ignored. Effective materialization names must be unique and
become the exact source destinations recorded in the frozen plan. These values
are authored requests.
Cast writes generated `sources.lock.glu` schema v2 beside `stone.glu`; each
Git entry contains a full commit ID and a required
`materialization_sha256`. Refresh it without rewriting authored Gluon:

```sh
cast recipe update ./stone.glu
```

A recipe which declares sources needs a current source lock before its
derivation can be planned. Schema v1 Git locks are intentionally unsupported;
run the explicit update command to regenerate them rather than relying on a
fallback or runtime digest.

Git submodules are not implicit sources. A locked Git commit containing a
Gitlink is rejected; declare each required checkout as its own typed source so
its URL, identity, and destination are visible in the plan. Lock refresh
exports the exact commit without Git administration data, rejects hard links
and special inodes, normalizes directories and executable/non-executable file
modes, and hashes raw paths, entry kinds, file bytes, executable state, and
symlink targets. Frozen setup performs the same export and rejects a digest
mismatch before the build container or analyzers start. Timestamps are
normalized to the plan's `source_date_epoch` but do not change the tree digest.
Archive copies likewise receive an independent cache inode, fixed mode, and
the frozen timestamp.

### Frozen builds are offline

Every byte fetched from outside the build root must be declared in `sources`
and admitted through `sources.lock.glu` before execution. The package-v3
`options.networking` field remains in the typed ABI as a possible future
fixed-output request, but setting it to `b.boolean.true` is currently a package
validation error. No valid frozen `PackageSpec` can enable in-build network
access.

Generated Cargo drafts therefore keep networking disabled. A draft is only a
starting point: any Cargo registry, Git, or vendor content required by the
build must be represented by locked sources rather than downloaded by a build
step.

## Build closure and derivation planning

`build.lock.glu` is also generated beside `stone.glu`. Schema v6 records the
exact reachable package/output closure, its used repository index snapshots,
platform roles, and separate policy-root, target, profile, toolchain, and
selected structural-builder identities. Each resolved provider records all
typed origins collected before request deduplication: selected
builder/native/build/check positions, output runtime edges, policy
source/field/index positions, exact job executable coordinates, and analyzer
roles. The builder fingerprint commits to
the complete target-selected builder, hooks, and package-profile key; the
executor ABI is a separate derivation execution-policy identity. The lock is
not an authored overlay. Reuse validates every
selected identity, platform component, and complete request-to-origin map
rather than trusting only the generated request-fingerprint field.

Create or refresh it while freezing a target-specific plan:

```sh
cast recipe plan ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8 \
    --update-lock
```

Use `--refresh-repositories` only together with `--update-lock`. Once the lock
is current, omit both flags to prove that the same request reproduces the same
canonical plan and derivation ID:

```sh
cast recipe plan ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

`recipe plan` prints the derivation ID, request fingerprint, plan counts, and
canonical plan bytes. `recipe explain` uses the current lock and prints the
recipe, source-lock, build-lock, request, policy, profile, and package-closure
provenance. For every provider it prints the request, exact locked
package/output, and every origin:

```sh
cast recipe explain ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

The source timestamp and job count are explicit because build scripts can
observe them. The derivation ID is SHA-256 over the canonical
`DerivationPlan`, including locked sources and dependencies, selected policy,
jobs and phases, environment, execution policy, pseudo-filesystems, outputs,
and timestamp. Derivation schema v13 includes the canonical build-lock origin
mapping, so changing only why an unchanged provider was requested changes the
derivation ID.

## Frozen execution

Build with the same explicit inputs used during planning:

```sh
cast build ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

The build requires a current `build.lock.glu`; pass `--update-lock` to resolve
and atomically refresh it, and add `--refresh-repositories` only when updating
the lock. Cast then exact-installs the locked package IDs, verifies the
locked repository snapshots, materializes the locked source identities, enters
the plan-defined container, runs `Executor` over frozen jobs and steps, and
packages through plan-owned analysis, collection, outputs, and derivation ID.
Binary manifest verification is host-only, and cleanup removes only paths
owned by the plan.

Frozen builds never inherit the generic container's compatibility mounts.
Proc and `/sys` are always absent, `/tmp` is empty, and repository policy may
select minimal or absent `/dev`. The default minimal `/dev` contains exactly
`null`, `zero`, and `full`; it has no host-dependent optional nodes. Frozen
networking is rejected, and its new network namespace retains the kernel's
default loopback state without running a host `ip` utility.

Mutable local files under the recipe `pkg/` directory are deliberately not
exposed to build steps. A future local-source ABI must hash their bytes and
destination into the derivation before those inputs can be supported safely.

`cast chroot ./stone.glu` is an explicitly impure interactive development
exception. It opens an existing build root without frozen planning or
execution and is outside the reproducibility guarantees above. It never
invokes, validates, or syncs package emission; files manually created by the
shell are not frozen build artifacts. Use it for investigation only, not as a
build path.

The legacy macro policy and `%action`/`%(definition)` parser have been removed.
Module-owned builder graphs and explicit literal `Shell` steps both freeze
through the typed build context; repository command/environment templates are
resolved during planning and there is no compatibility expansion pass.

See [`examples/gluon/package_v3.glu`](examples/gluon/package_v3.glu) and
[`examples/gluon/package_v3_stone.glu`](examples/gluon/package_v3_stone.glu)
for runnable factory, scope, output, and patch examples.
