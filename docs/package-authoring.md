<!--
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Package authoring

Boulder packages are pure Gluon programs evaluated through
`boulder.package.v2`. A recipe may import local modules and call functions, but
Rust receives one concrete, validated `PackageSpec`; it never retains a Gluon
closure. The former `boulder.recipe.v1` ABI is not a compatibility path.

This guide describes the current authoring, planning, and execution contract.
`recipe plan`, `recipe explain`, and normal `boulder build` all use the same
target-specific frozen derivation model.

## A package factory and explicit scope

Put reusable package construction in a function. Its argument is an ordinary
record containing symbolic dependencies selected by the caller:

```gluon
// package.glu
let b = import! boulder.package.v2
let cmake = import! boulder.builders.cmake.v1

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
                "0123456789abcdef",
        ],
        .. base
    }
```

The root `stone.glu` supplies the scope and therefore produces the concrete
package value:

```gluon
let b = import! boulder.package.v2
let make = import! "./package.glu"

make {
    pkgconf = b.dep.binary "pkgconf",
    zlib = b.dep.pkgconfig "zlib",
}
```

There is no `callPackage`-style argument-name reflection. A missing scope field
is a Gluon type error. Scope values remain symbolic during evaluation; Moss
provider resolution occurs later when Boulder creates `build.lock.glu`.
Scopes remain ordinary, nonrecursive imported Gluon records; there is no
second Rust `PackageSet` ABI or hidden alias-provenance layer. Local output
cycles are rejected during package validation, and cycles in the resolved Moss
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
used by Boulder, Moss, and package conversion. Local output references are
validated before planning, including missing outputs and cycles.

## Standard builders

Import one standard builder module and start from its `defaults` record when
overriding typed settings:

| Module | Settings | Structural phases |
|---|---|---|
| `boulder.builders.cmake.v1` | `flags`, `run_tests` | configure, build, install, test |
| `boulder.builders.meson.v1` | `flags`, `run_tests` | setup, build, install, test |
| `boulder.builders.cargo.v1` | `features`, `binaries`, `run_tests` | environment, build, install, test |
| `boulder.builders.autotools.v1` | `flags`, `run_tests` | configure, build, install, test |

Booleans use `b.boolean.true` and `b.boolean.false`. For example:

```gluon
let b = import! boulder.package.v2
let cargo = import! boulder.builders.cargo.v1

cargo.builder {
    features = ["cli", "tls"],
    binaries = ["hello", "helloctl"],
    run_tests = b.boolean.true,
}
```

Standard builders declare their required tools and produce Rust `StepSpec`
variants. They do not author or lower through `%cmake`, `%meson`, `%cargo`,
`%configure`, or `%make` action strings.

## Typed phases and hooks

The standard builder owns its phase body. Hooks add explicit steps before or
after it:

```gluon
hooks = b.hooks {
    pre_build = [b.step.shell "generate-sources"],
    post_install = [
        b.step.shell "ln -s hello %(installroot)/usr/bin/hi",
    ],
    environment = [
        b.step.shell "HELLO_FEATURES=cli; export HELLO_FEATURES",
    ],
    .. b.defaults.hooks
}
```

Available hook positions are `pre_` and `post_` for `setup`, `build`, `check`,
`install`, and `workload`, plus `environment`. Hook and builder step order is
preserved, but the executor may spawn a separate `bash -c` process for each
frozen shell step. Filesystem effects persist; process-local variables and
working-directory changes must not be assumed to cross step boundaries. Put
shared variables in the phase environment and use explicit working paths.

The public Gluon step constructors currently expose `b.step.shell` and the
structural `b.step.cargo_fetch`. Standard builder steps are generated by the
builder module rather than authored individually.

## Custom shell builders

Use a custom builder only when the build cannot be represented by a standard
builder. Every phase is an explicit `b.phase` and every executable program must
also be declared in `required_tools`:

```gluon
let b = import! boulder.package.v2

let scripts = b.scripts {
    setup = b.phase [b.step.shell "zig build --fetch"],
    build = b.phase [b.step.shell "zig build -Doptimize=ReleaseSafe"],
    check = b.phase [b.step.shell "zig build test"],
    install = b.phase [
        b.step.shell "zig build install --prefix %(installroot)/usr",
    ],
    .. b.defaults.scripts
}

{
    builder = b.builder.custom scripts [b.dep.binary "zig"],
    .. b.mk_package (b.meta {
        pname = "zig-hello",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/zig-hello",
        license = ["MIT"],
    })
}
```

`Shell` is the only escape hatch which may enter the transitional legacy
script parser. `%action` macros remain accepted there for migration, but new
packages should call tools directly and declare them in `required_tools`.
Definitions such as `%(installroot)` and `%(workdir)` also remain transitional;
they will be replaced by typed layout/environment values. Standard builders do
not depend on authored `%action` strings.

## Outputs

Every package must contain exactly one output named `out`. Additional output
names are local names; Boulder currently lowers `dev` to `<pname>-dev` at the
internal packaging boundary.

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
    b.source.archive "https://example.invalid/hello.tar.xz" "sha256-hex",
    b.source.git "https://example.invalid/hello.git" "v1.0.0",
]
```

Use `archive_with` to set `rename`, `strip_dirs`, `unpack`, or `unpack_dir`, and
`git_with` to set `clone_dir`. These are authored requests. Boulder writes the
resolved, generated `sources.lock.glu` beside `stone.glu`; Git locks contain a
full commit ID. Refresh it without rewriting authored Gluon:

```sh
boulder recipe update ./stone.glu
```

A recipe which declares sources needs a current source lock before its
derivation can be planned.

## Build closure and derivation planning

`build.lock.glu` is also generated beside `stone.glu`. It records the exact
package/output closure, base state, repository index snapshots, platform roles,
and selected policy, profile, toolchain, and builder identities. It is not an
authored overlay.

Create or refresh it while freezing a target-specific plan:

```sh
boulder recipe plan ./stone.glu \
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
boulder recipe plan ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

`recipe plan` prints the derivation ID, request fingerprint, plan counts, and
canonical plan bytes. `recipe explain` uses the current lock and prints the
recipe, source-lock, build-lock, request, policy, profile, and package-closure
provenance:

```sh
boulder recipe explain ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

The source timestamp and job count are explicit because build scripts can
observe them. The derivation ID is SHA-256 over the canonical
`DerivationPlan`, including locked sources and dependencies, selected policy,
jobs and phases, environment, execution policy, outputs, and timestamp.

## Frozen execution

Build with the same explicit inputs used during planning:

```sh
boulder build ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

The build requires a current `build.lock.glu`; pass `--update-lock` to resolve
and atomically refresh it, and add `--refresh-repositories` only when updating
the lock. Boulder then exact-installs the locked package IDs, verifies the
locked repository snapshots, materializes the locked source identities, enters
the plan-defined container, runs `Executor` over frozen jobs and steps, and
packages through plan-owned analysis, collection, outputs, and derivation ID.
Binary manifest verification is host-only, and cleanup removes only paths
owned by the plan.

Mutable local inputs referenced through `%(pkgdir)` under the recipe `pkg/`
directory are deliberately rejected before freeze. A future local-source ABI
must hash their bytes and destination into the derivation before those inputs
can be supported safely.

The remaining pre-freeze migration is the macro-definition parser. Typed
policy, layout, tuning, and environment values must cover its semantics before
the compatibility parser can be deleted.

See [`examples/gluon/package_v2.glu`](examples/gluon/package_v2.glu) and
[`examples/gluon/package_v2_stone.glu`](examples/gluon/package_v2_stone.glu)
for runnable factory, scope, output, and patch examples.
