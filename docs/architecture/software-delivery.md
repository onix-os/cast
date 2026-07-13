<!--
# SPDX-FileCopyrightText: 2025 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Software delivery architecture

This is inherited Serpent OS/AerynOS architecture retained by the Onix hard
fork. The quotations document the origins of the Stone and state design; current
Onix-specific package authoring and derivation contracts are documented in
[`../package-authoring.md`](../package-authoring.md) and
[`declarative-stone-contracts.md`](declarative-stone-contracts.md).

## Software package metadata: manifest.*.bin

The `manifest.${ARCH}.bin` files contain the metadata consumed by the tooling.
The `manifest.${ARCH}.jsonc` files are for review and human-readable insight;
the tooling ignores them. Current Cast metadata includes the evaluated
recipe fingerprint and the canonical derivation ID so package provenance can
be related to its frozen plan.

    **Ikey Doherty**
    > our manifest.*.bin format is just a .stone in disguise
    > containing only a metadata payload with special fields
    > and the stone archive type flag is set to buildmanifest
    > sneaksy
    > (in fact, our repo format is also just a set of meta payloads in a stone file..)
    > but its also strongly typed, fixed headers, version agnostic header unpack and compressed with zstd with CRC checks
    > soo. a little less weak than sounding
    > crc is actually xxh64 iirc

## Software distribution via *.stone packages

Onix retains the custom `stone` format for fast, deduplicated transmission and
installation. A Stone package ID or payload hash proves artifact identity and
integrity. It is distinct from the derivation ID, which hashes the canonical
inputs and requested build semantics before execution.

    > **Ikey Doherty**
    > Context: we dont mix layout + metadata (unlike in alpine, where tar records are used for metadata)
    > in fact we explicitly separate them
    > so a "normal" stone file has a meta payload with strongly typed/tagged key value pairs/sets
    > a content payload which is every unique file concatenated into a "megablob" and compressed singly
    > an index payload which is a jump table into offsets in the unpacked content payload
    > to allow the xxhash128 keying
    > ie "position one is hash xyz"
    > and lastly there is the layout payload which is a meta-ish payload containing a set of records that define how the package is laid out on disk when installed
    > so the paths, file types, modes, link targets, permissions
    > and optionally for regular files, the xxh128 hash
    > so when we "cache" / install a package, in reality we're ripping the content payload out, then using the index payload to shard it into the unique assets in the store to build up the content addressable storage
    > we then merge the entries from metapayload + layoutpayload into the DBs
    > and we use the unique package "id" to key it, ie the hash for the `.stone`
    > In summary: "A lot more than a single tar file can do."

Forge retains the corresponding state model internally. It maps explicit and
transitive selections to package IDs, builds an in-memory VFS, detects
filesystem and symlink conflicts before mutation, stages a complete system,
runs transaction triggers in an isolated environment, and activates the new
tree atomically. Cast is the only public command and product name for these
operations.
