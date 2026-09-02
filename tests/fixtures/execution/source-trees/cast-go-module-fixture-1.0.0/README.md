# Cast Go module fixture

This source tree is self-authored for Cast's offline execution tests. It uses a
complete vendored copy of the self-authored `fixtures.invalid/cast/go-message`
module. The checked-in `go.sum` records the module archive used to create that
vendor tree; the final module has no `replace` directive and cannot bypass the
vendored dependency.

The executable exposes `--self-test` so both the recipe and the supplemental
host lane prove that the compiled program actually contains and uses the
vendored dependency identity.
