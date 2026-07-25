
# Package authoring

Cast packages are authored through `cast.authored.v1`, a pure type +
constructor layer with no defaults and no builder-expansion or merge logic of
its own, and evaluated to one concrete, validated `PackageSpec`; Rust never
retains a Gluon closure. A recipe may import local modules and call functions
to build that record, but every field of the authored record is named
directly — there is no template-inheritance stack and no hidden repository
merge. Lua authors the identical shape as a plain table, and all package-ABI
defaults plus builder-request lowering live in shared Rust
(`stone_recipe::package::{lower, lower_builder, default_output_set}`), so
Gluon and Lua are interchangeable thin syntaxes over the same `PackageSpec`
and either can be removed without losing authoring.
`crates/stone_recipe/tests/authoring_independence.rs` proves this: it authors
the same non-trivial package once through the Gluon evaluator with Lua
nowhere in the path, once through the Lua evaluator with Gluon nowhere in the
path, and asserts the two independently produced `PackageSpec`s are
byte-identical. The retired recipe-v1 ABI is not a compatibility path.

This guide describes the current authoring, planning, and execution contract.
`cast recipe plan`, `cast recipe explain`, and normal `cast build` all use the same
target-specific frozen derivation model.

## A package factory and explicit scope

Put reusable package construction in a function. Its argument is an ordinary
record containing symbolic dependencies selected by the caller:

```gluon
// package.glu
let a = import! cast.authored.v1

\scope ->
    {
        meta = {
            pname = "hello",
            version = "1.0.0",
            release = 1,
            homepage = "https://example.invalid/hello",
            license = ["MPL-2.0"],
        },
        builder = a.builder.cmake {
            flags = ["-DBUILD_TESTS=ON"],
            run_tests = a.true,
        },
        sources = [
            a.source.archive
                "https://example.invalid/hello-1.0.0.tar.xz"
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ],
        native_build_inputs = [scope.pkgconf],
        build_inputs = [scope.zlib],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }
```

The root `stone.glu` supplies the scope and therefore produces the concrete
package value:

```gluon
let a = import! cast.authored.v1
let make = import! "./package.glu"

make {
    pkgconf = a.dep.binary "pkgconf",
    zlib = a.dep.pkgconfig "zlib",
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

Every authored record is structural and total: Gluon requires all fourteen
top-level fields (`meta`, `builder`, `sources`, `native_build_inputs`,
`build_inputs`, `check_inputs`, `outputs`, `options`, `profiles`,
`architectures`, `tuning`, `emul32`, `mold`, `hooks`) to be present, and a
default is selected explicitly with `a.unset` or `a.outputs.default`. Lua has
no such requirement: `LuaAuthoredPackage` marks every field but `meta` and
`builder` `#[serde(default)]`, so a Lua table simply omits any field taking
its shared-Rust default.

## Dependency roles

Use typed dependency constructors rather than provider strings:

```gluon
a.dep.package "zlib"
a.dep.output (a.package_ref "llvm") "clang"
a.dep.binary "cmake"
a.dep.system_binary "ldconfig"
a.dep.pkgconfig "openssl"
a.dep.pkgconfig32 "zlib"
a.dep.soname "libz.so.1"
a.dep.cmake "Qt6"
a.dep.python "setuptools"
a.dep.interpreter "/usr/lib/ld-linux-x86-64.so.2(x86_64)"
```

In Lua the same capabilities are tagged tables: `{ kind = "package", value = {
name = "zlib" } }`, `{ kind = "output", value = { package = { name = "llvm" },
output = "clang" } }`, `{ kind = "binary", value = "cmake" }`, and so on for
`system_binary`, `pkg_config`, `pkg_config32`, `soname`, `cmake`, `python`, and
`interpreter`.

`interpreter` is the exact architecture-qualified ELF `PT_INTERP` loader
capability emitted by package analysis. Script runtimes such as `python3`,
`bash`, and `sh` are executable capabilities and use `a.dep.binary` instead.

Put them in the field matching their purpose:

- `builder.required_tools`: executable Package, Output, Binary, or
  SystemBinary capabilities invoked by the selected builder;
- `native_build_inputs`: build-platform capabilities;
- `build_inputs`: target libraries, headers, tools, or other build-time
  capabilities;
- `check_inputs`: capabilities used only by checks;
- `outputs[*].runtime_inputs`: runtime relations of one emitted output.

Native, build, and check inputs deliberately accept every typed capability:
cross builds can require both tools and libraries in those roles. Runtime
inputs reject development-only CMake, PkgConfig, and PkgConfig32 metadata.
Output conflicts remain provider relations and accept every provider-capable
kind.

The shared `stone::relation` model is the canonical parser and representation
used by Cast and package conversion. Local output references are
validated before planning, including missing outputs and cycles.

A binary capability may be implemented by a package-owned symlink whose
target is supplied by another package in the same frozen closure. Cast pins
the declared entry-point provider, follows each lexical `/usr` symlink only
through the frozen layout index, and requires exactly one provider for every
handoff. Missing or ambiguous targets fail before the build container starts;
the host filesystem is never used to complete the chain.

## Standard builders

`builder` is a request, not a template: name the standard build system by
kind and Rust's shared `lower_builder` expands it into required tools, an
environment marker, and the typed phase graph.

| Request (Gluon) | Lua `kind` | Settings | Structural phases | Environment marker |
|---|---|---|---|---|
| `a.builder.cmake { .. }` | `"cmake"` | `flags`, `run_tests` | configure, build, install, test | CMake |
| `a.builder.meson { .. }` | `"meson"` | `flags`, `run_tests` | setup, build, install, test | Meson |
| `a.builder.cargo { .. }` | `"cargo"` | `features`, `binaries`, `run_tests` | build, install, test | Cargo |
| `a.builder.autotools { .. }` | `"autotools"` | `flags`, `run_tests` | configure, build, install, test | Autotools |

Booleans use `a.true` and `a.false`. For example:

```gluon
let a = import! cast.authored.v1

a.builder.cargo {
    features = ["pcre2", "unicode"],
    binaries = ["cargo-hello"],
    run_tests = a.true,
}
```

The equivalent minimal Lua table omits any field taking its default
(`flags`/`features`/`binaries` default to `{}`, `run_tests` defaults to
`true`):

```lua
builder = { kind = "cargo", features = { "pcre2", "unicode" }, binaries = { "cargo-hello" } }
```

Gluon has no such per-field serde default — `BuilderRequest`'s Cmake/Meson/
Cargo/Autotools variants are ordinary Gluon records, so `flags`/`features`/
`binaries`/`run_tests` must all be written explicitly, as in the table above.

`lower_builder` expands the selected request into the same typed step phases
regardless of which language authored it: symbolic required-tool
capabilities, an environment marker, the ordered typed phase graph, and its
supported hook surface. Repository policy owns only the command templates for
those typed steps and the bindings selected by the marker; Rust does not
invent phases or a second tool list. No builder authors or lowers through
`%cmake`, `%meson`, `%cargo`, `%configure`, or `%make` action strings.

Use `a.builder.custom <BuilderSpec>` or `a.builder.shell <scripts> <tools>`
(Lua: `{ kind = "custom", spec = { .. } }`) when the build cannot be
represented by a standard request — see "Custom shell builders" below.

## Typed phases and hooks

The standard builder owns its phase body and declares which hook positions it
supports. Package or profile hooks add explicit steps before or after that
body. The `hooks` field itself takes `a.unset` to select the empty hook set,
or `a.some.hooks` wrapping an `a.hooks { .. }` record built on top of
`a.empty.hooks` for the phases left alone:

```gluon
hooks = a.some.hooks (a.hooks {
    pre_build = [a.step.run (a.program.binary "generate-sources") []],
    post_install = [
        a.step.shell_with {
            interpreter = a.program.binary "bash",
            declared_programs = [a.program.binary "ln"],
            script = r#"ln -s hello "${CAST_INSTALL_ROOT}${CAST_BINDIR}/hi""#,
        },
    ],
    .. a.empty.hooks
})
```

The current standard modules support `pre_` and `post_` positions for `setup`,
`build`, `check`, `install`, and `workload`; unsupported populated hooks are a
package-validation error. Hook and builder step order is preserved, but every
frozen shell step runs in its own declared interpreter process. `a.step.shell`
is shorthand for a Gluon-authored `/usr/bin/bash` capability with no additional
programs. Filesystem effects persist; process-local variables, shell options,
and working-directory changes never cross step boundaries. In particular, an
`export` in `pre_check` ends with that hook process and does not configure the
standard builder's following `CMakeTest` (or any other standard builder) step.
Put an environment assignment directly on the command which consumes it in the
same hook or custom-builder shell step. If a standard test command itself
requires extra environment variables, use a custom builder whose `check` phase
contains that command; a hook cannot mutate the standard builder step, and the
current package ABI has no per-step environment override.

The package ABI exposes the typed standard-step constructors used by
`lower_builder`'s standard-request expansion, plus `a.step.run`, `a.step.run_built`, `a.step.shell`, and
`a.step.shell_with` for explicit custom work. `a.step.run` binds an external
program to the locked package capability that provides it. A native executable
produced inside the current build tree instead uses
`a.step.run_built (a.program.built "relative/path")`; freezing binds that
normalized relative path beneath the exact phase working directory without
inventing a package dependency. The current native format is Linux ELF, and
the retained descriptor remains close-on-exec when passed directly to
`execveat`. Scripts must use an explicit `a.step.shell` or `a.step.shell_with`;
a descriptor-executed shebang fails closed without reopening the public path.
The ABI deliberately has no `cargo_fetch` step: a frozen
build cannot resolve or download Cargo dependencies. Cargo inputs must already
be present in a locked source, with Cargo configured to use that vendored tree;
the standard Cargo builder runs with `--frozen`. Package authors normally
select a standard request rather than rebuilding its phase graph step by step.

## Custom shell builders

Use a custom builder only when the build cannot be represented by a standard
request. Every phase is an explicit `a.phase`. A direct `Run` binds one program
to the dependency capability which provides it; a `Shell` binds its interpreter
and every non-builtin program invoked by the script. `a.builder.shell phases
tools` is sugar for a `Custom` `BuilderSpec` with no extra environment marker
and every hook position supported; use `a.builder.custom <BuilderSpec>`
directly when the escape hatch itself needs a non-default `environment` or
`supported_hooks`:

```gluon
let a = import! cast.authored.v1

let pname = "zig-vector"
let zig = a.program.binary "zig"

// build.zig.zon refers only to the locked vendor tree. Zig receives explicit
// local and global caches in every phase because process environments never
// leak from one frozen step into the next.
let phases = a.scripts {
    setup = a.phase [
        a.step.shell r#"test -f build.zig
test -f build.zig.zon
test -d vendor"#,
    ],
    build = a.phase [
        a.step.shell_with {
            interpreter = a.program.binary "bash",
            declared_programs = [zig],
            script = r#"ZIG_GLOBAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-global-cache" \
ZIG_LOCAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-local-cache" \
zig build -Doptimize=ReleaseSafe"#,
        },
    ],
    check = a.phase [
        a.step.shell_with {
            interpreter = a.program.binary "bash",
            declared_programs = [zig],
            script = r#"ZIG_GLOBAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-global-cache" \
ZIG_LOCAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-local-cache" \
zig build test -Doptimize=ReleaseSafe"#,
        },
    ],
    install = a.phase [
        a.step.shell_with {
            interpreter = a.program.binary "bash",
            declared_programs = [zig],
            script = r#"ZIG_GLOBAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-global-cache" \
ZIG_LOCAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-local-cache" \
zig build install -Doptimize=ReleaseSafe --prefix "${CAST_INSTALL_ROOT}${CAST_PREFIX}""#,
        },
    ],
    .. a.empty.scripts
}

{
    meta = {
        pname,
        version = "0.8.0",
        release = 1,
        homepage = "https://example.invalid/zig-vector",
        license = ["MIT"],
    },
    builder = a.builder.shell phases [a.dep.binary "zig"],
    sources = [
        a.source.archive
            "https://example.invalid/zig-vector-0.8.0-vendored.tar.xz"
            "23456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef01",
    ],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = ["x86_64", "aarch64"],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}
```

This graph has no dependency-download or fetch phase. The admitted source
archive supplies the complete vendor tree before execution, `build.zig.zon`
resolves only local paths, and every Zig process repeats its cache assignments
because no process environment crosses a step boundary. A URL dependency or an
incomplete vendor tree must fail the offline build; it is not a reason to add
an in-build fetch command.

`Shell` is an explicit, literal escape hatch. It never enters the former macro
parser: `%name` and `%(name)` have no special meaning. `a.program.binary` and
`a.program.system_binary` construct canonical `/usr/bin` and `/usr/sbin`
bindings. `a.program.package` and `a.program.output` bind an arbitrary
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

The `outputs` field is a choice, not a merge target: `a.outputs.default`
selects the shared Rust `default_output_set` (an opaque marker — `Default`,
`Explicit`, and `WithRoot` are a dedicated, non-`Optional` choice type, not an
overlay Gluon can inspect), `a.outputs.explicit [ .. ]` replaces the whole set
with an authored list, and `a.outputs.with_root <output>` overlays a custom
root output onto the default split-output set, with the overlay itself
applied by shared Rust:

```gluon
let a = import! cast.authored.v1
let package_policy = import! "./package_policy.glu"

{
    // ...
    outputs = a.outputs.with_root (package_policy (a.output "out")),
    // ...
}
```

where `package_policy.glu` decorates the base output record:

```gluon
let a = import! cast.authored.v1

\output ->
    let output = a.output_with output
    {
        summary = a.optional.set "Recipe composed with an imported policy",
        runtime_inputs = [a.dep.package "ca-certificates"],
        .. output
    }
```

These are versioned package-ABI defaults: `default_output_set` is the same
nine-output split (`out`, `docs`, `devel`, `dbginfo`, `libs`, `32bit`,
`32bit-devel`, `32bit-dbginfo`, `demos`) for every recipe, computed once in
shared Rust rather than duplicated per-language, and an incompatible change to
it requires a new package ABI version. A recipe that needs to *extend* the
default set (for example, appending an extra "tools" output) has no partial
append: `a.outputs.default` is opaque at authoring time, so it must reproduce
the same nine outputs explicitly through `a.outputs.explicit` and append its
own — see `docs/examples/gluon/packages/factory-override/package.glu` and
`docs/examples/gluon/packages/layered-overrides/layers.glu` for a full
worked copy of that default set plus an appended output.

A fully custom output list uses `a.outputs.explicit` directly:

```gluon
let root = {
    summary = a.optional.set "Command-line tools",
    paths = [
        a.path.exe "/usr/bin/split-demo",
        a.path.any "/usr/share/man/man1",
    ],
    .. a.output "out"
}

let libraries = {
    summary = a.optional.set "Runtime libraries",
    paths = [a.path.any "/usr/lib/libsplit-demo.so.*"],
    .. a.output "libs"
}

{
    // ...
    outputs = a.outputs.explicit [root, libraries],
    // ...
}
```

Materializable path constructors are `a.path.any`, `a.path.exe`, and
`a.path.symlink`. The `cast.authored.v1` `a.path.special` constructor remains
reserved so the versioned ABI does not change in place, but concrete package
validation rejects it: immutable package layouts cannot contain devices,
FIFOs, or sockets. Package a `/usr/lib/tmpfiles.d/*.conf` regular file when
such runtime state must be created during system activation. Outputs can also
set `description`, `provides_exclude`, `runtime_exclude`, and typed
`conflicts`.

## Composition and overrides

Change a factory argument before construction by updating its scope, the same
way `make { pkgconf = .., zlib = .. }` above supplies concrete dependencies:

```gluon
let package = make_package {
    tls = a.dep.pkgconfig "libressl",
    .. inputs
}
```

Change a completed package with an ordinary Gluon record update over the
authored base — there is no patch ADT (`b.package_patch`, `b.override_attrs`,
`b.patch.array.replace`/`append`/`prepend`, `b.patch.keep`/`set`) to route
through. Fields left out are inherited from `base` unchanged; there is no
implicit old-attribute merge or stale-source fallback:

```gluon
let a = import! cast.authored.v1
let base = import! "./package.glu"

{
    meta = {
        pname = "override-release",
        version = "2.1.0",
        release = 4,
        homepage = "https://example.invalid/override-release",
        license = ["MIT"],
    },
    sources = [
        a.source.archive_with {
            url = "https://example.invalid/override-release-2.1.0.tar.xz",
            hash = "4444444444444444444444444444444444444444444444444444444444444444",
            rename = a.optional.set "override-release-2.1.0.tar.xz",
            strip_dirs = a.optional.set 1,
            unpack = a.true,
            unpack_dir = a.optional.set "override-release-2.1.0",
        },
    ],
    .. base
}
```

Layer several independent transformations by chaining ordinary
`PackageSpec -> PackageSpec` functions instead of an order-dependent
attribute-merge stack. Composing a list field (rather than replacing it)
is plain Gluon array concatenation:

```gluon
let a = import! cast.authored.v1
let arrays = import! std.array.prim

let release_options = a.options {
    toolchain = a.toolchain.llvm,
    cspgo = a.false,
    samplepgo = a.false,
    debug = a.false,
    strip = a.true,
    networking = a.false,
    compressman = a.true,
    lastrip = a.true,
}

let security = \package -> {
    meta = package.meta,
    builder = package.builder,
    sources = package.sources,
    native_build_inputs = arrays.append [a.dep.binary "hardening-check"] package.native_build_inputs,
    build_inputs = package.build_inputs,
    check_inputs = package.check_inputs,
    outputs = package.outputs,
    options = a.some.options release_options,
    profiles = package.profiles,
    architectures = package.architectures,
    tuning = arrays.append [a.named "harden" a.tuning.enable] package.tuning,
    emul32 = package.emul32,
    mold = package.mold,
    hooks = package.hooks,
}

security base
```

Whether a layer keeps a field (copy it through, or omit it under `.. base`
record update), replaces it (write a new literal, including `[]`), or extends
it (`arrays.append` the addition and the existing value) is visible directly
in the function body — there is no separate `keep`/`replace`/`prepend`/`append`
verb layer to reach for. See `docs/examples/gluon/packages/release-override`,
`docs/examples/gluon/packages/factory-override`, and
`docs/examples/gluon/packages/layered-overrides` for complete, runnable
copies of these three patterns.

## Options, profiles, and tuning

`options` takes `a.unset` to select `OptionsSpec`'s shared-Rust default, or
`a.some.options (a.options { .. })` to set every field explicitly. Toolchains
are `a.toolchain.llvm` and `a.toolchain.gnu`; Boolean fields use `a.true` /
`a.false`:

```gluon
options = a.some.options (a.options {
    toolchain = a.toolchain.gnu,
    cspgo = a.false,
    samplepgo = a.false,
    debug = a.false,
    strip = a.true,
    networking = a.false,
    compressman = a.true,
    lastrip = a.true,
}),
```

Tuning entries use `a.named "<key>" <value>` with `a.tuning.enable`,
`a.tuning.disable`, or `a.tuning.config "<value>"`:

```gluon
tuning = [
    a.named "harden" a.tuning.enable,
    a.named "lto" (a.tuning.config "thin"),
    a.named "nosemantic" a.tuning.disable,
    a.named "optimize" (a.tuning.config "speed"),
],
```

Target-specific profiles are entries in `profiles`, each a `name`, `builder`,
`hooks`, and native/build/check input list. A profile's `builder` field is a
fully lowered `BuilderSpec`, not the top-level `BuilderRequest` — `ProfileSpec`
passes through `lower` untouched, so a profile that wants the same expansion
`a.builder.cmake { .. }` gets at the top level has to author that step graph
by hand (or through a small local helper), the way the legacy
`cast.builders.cmake.v2` module used to:

```gluon
let cmake_profile_builder = \flags run_tests ->
    let check_steps =
        match run_tests with
        | True -> [a.step.cmake_test]
        | False -> []
    {
        required_tools = [a.dep.binary "sh", a.dep.binary "ninja"],
        environment = [a.environment.cmake],
        phases = {
            setup = a.phase [a.step.cmake_configure flags],
            build = a.phase [a.step.cmake_build],
            install = a.phase [a.step.cmake_install],
            check = a.phase check_steps,
            workload = a.phase [],
        },
        supported_hooks = a.hook_support.all,
    }

let emul32_profile = {
    name = "emul32",
    builder = cmake_profile_builder ["-DCMAKE_INSTALL_LIBDIR=lib32", "-DENABLE_TOOLS=OFF"] True,
    hooks = a.empty.hooks,
    native_build_inputs = [a.dep.binary "nasm"],
    build_inputs = [a.dep.pkgconfig32 "zlib"],
    check_inputs = [a.dep.binary "file"],
}
```

## Source resolution and `sources.lock.glu`

Declare archives or Git requests in `sources`:

```gluon
sources = [
    a.source.archive
        "https://example.invalid/hello.tar.xz"
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    a.source.git "https://example.invalid/hello.git" "v1.0.0",
]
```

Archive hashes are exactly 64 lowercase hexadecimal characters. They bind the
authored request, generated source lock, frozen derivation, and fetched bytes
to one canonical SHA-256 identity.

Use `archive_with` to set `rename`, `strip_dirs`, `unpack`, or `unpack_dir`, and
`git_with` to set `clone_dir`:

```gluon
a.source.git_with {
    url = "https://example.invalid/hello.git",
    git_ref = "v1.0.0",
    clone_dir = a.optional.set "hello-source",
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

### Archive extraction contract

When an archive has `unpack = a.true`, derivation schema v16 records a
built-in `ExtractArchive` step in the prepare-phase body. The step refers to one
locked archive by index and freezes its normalized relative destination and
`strip_dirs` value. Plan validation rejects a non-archive source, extraction in
any hook or other phase, more than 128 stripped components, equal or nested
archive destinations, and any archive destination that is equal to, above, or
below a locked Git checkout directory. The step is executed by Cast itself, not by
`bsdtar`, `tar`, or a package-provided command, so there is no undeclared
executable or format fallback.

The decoder selects compression from the bytes, not the filename suffix:

| Accepted stream | Common suffixes |
|---|---|
| Plain tar | `.tar` |
| Gzip-compressed tar | `.tar.gz`, `.tgz` |
| XZ-compressed tar | `.tar.xz`, `.txz` |
| Zstandard-compressed tar with standard frame magic | `.tar.zst`, `.tar.zstd` |

Bzip2, ZIP, 7z, RAR, lzip, Unix compress (`.Z`), and every other compression or
container format are unsupported. A recognized compressed stream must decode
to a valid tar stream; a suggestive filename does not change that requirement.

Before touching the destination, Cast verifies the locked digest and scans the
decoded tar into a bounded canonical manifest. Regular files, directories,
non-escaping relative symlinks, and backward hard links to an already admitted
regular file or hard link are supported. It rejects unsafe or duplicate paths,
escaping links, forward or stripped-away hard-link targets, sparse files,
special inodes, incompatible path topology, global PAX state, and unsupported
PAX keys (including PAX `size` overrides). Compressed and decoded bytes,
decoder working memory, entries, aggregate path bytes, path and link depth,
per-entry and aggregate extension data, individual and aggregate file sizes,
materialized nodes, and wall time all have fixed ceilings. Extraction count,
compressed bytes, decoded bytes, entries, path data, extension data, logical
and physical file bytes, nodes, and wall time also have aggregate limits shared
by every archive step in one derivation.

Cast copies and hashes the locked source once into a sealed immutable snapshot;
both scans and extraction read only that snapshot, so a same-UID host writer
cannot alternate archive contents between verification passes. Extraction
writes only beneath a private descriptor-rooted stage. Cast descriptor-pins and
normalizes nested destination parents, normalizes the stage to exact modes and
`source_date_epoch`, scans the snapshot a second time, requires both canonical
manifests to match, and publishes without replacing an existing destination.
Pre-publication failures trigger bounded stage cleanup. A failure after the
atomic rename (for example, a durability-sync error) aborts the build but never
mistakes the published tree for the private stage and purges it through the
obsolete stage name.

### Frozen builds are offline

Every byte fetched from outside the build root must be declared in `sources`
and admitted through `sources.lock.glu` before execution. The authored
`options` field's `networking` setting remains in the typed ABI as a possible
future fixed-output request, but setting it to `a.true` is currently a package
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
and timestamp. Derivation schema v16 includes the canonical build-lock origin
mapping and typed built-in archive extraction, so changing only why an
unchanged provider was requested, or changing an archive source, destination,
or strip count, changes the derivation ID.

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

The host-side supervisor must itself run under a systemd cgroup-v2 unit with
`Delegate=cpu memory pids` and `DelegateSubgroup=cast-supervisor`. Cast reads a
single bounded `0::` entry from `/proc/self/cgroup`, requires its normalized
absolute path to end exactly in `/cast-supervisor`, and descriptor-authenticates
the non-root parent below `/sys/fs/cgroup`. There is no environment override,
numeric PID migration, or legacy-clone fallback. A frozen derivation starts
atomically in a terminal leaf limited to 4096 aggregate PIDs, 32 GiB memory,
zero swap, and `execution.jobs` CPUs per 100 ms period. These deliberately
generous finite ceilings belong to executor policy and do not change the
derivation ID; a host without the delegation fails before payload execution.

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
Standard and custom builder graphs alike freeze through the typed build
context; repository command/environment templates are resolved during
planning and there is no compatibility expansion pass.

See [`examples/gluon/stone.glu`](examples/gluon/stone.glu) and
[`examples/gluon/composed-stone.glu`](examples/gluon/composed-stone.glu) for
minimal runnable authored-package examples, and
[`examples/gluon/packages/`](examples/gluon/packages/) for a corpus of over
sixty complete recipes covering every standard builder, custom-step,
hook, factory/override, profile, and output pattern described above.
