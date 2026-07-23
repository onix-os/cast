-- SPDX-FileCopyrightText: 2026 AerynOS Developers
-- SPDX-License-Identifier: MPL-2.0

-- Durable migration authority (Phase L8). One committed row is the single
-- authority that a state-owned declaration slot has been converted to Lua.
-- Immutable content-addressed blobs live under
-- <root>/.cast/declaration-migrations/v1/blobs/<sha256>.lua; only a committed
-- row here selects one. A row whose state marker, original source, converted
-- blob, or evaluation identity no longer matches must fail closed.
CREATE TABLE declaration_migrations (
    state_id INTEGER NOT NULL,
    -- Canonical slot identity (e.g. the fixed logical name), stable across
    -- engines.
    logical_slot TEXT NOT NULL CHECK (length(logical_slot) > 0),
    catalog_schema_version INTEGER NOT NULL CHECK (catalog_schema_version >= 1),
    -- Authenticated state-tree marker binding the row to its immutable state.
    state_tree_marker BLOB NOT NULL CHECK (
        typeof(state_tree_marker) = 'blob' AND length(state_tree_marker) = 32
    ),
    original_language TEXT NOT NULL CHECK (length(original_language) > 0),
    original_logical_path TEXT NOT NULL CHECK (length(original_logical_path) > 0),
    original_sha256 BLOB NOT NULL CHECK (
        typeof(original_sha256) = 'blob' AND length(original_sha256) = 32
    ),
    migrated_language TEXT NOT NULL CHECK (length(migrated_language) > 0),
    -- Content address of the immutable converted blob; equals its filename.
    migrated_blob_sha256 BLOB NOT NULL CHECK (
        typeof(migrated_blob_sha256) = 'blob' AND length(migrated_blob_sha256) = 32
    ),
    -- Canonical evaluation-identity bytes of the migrated declaration.
    evaluation_identity BLOB NOT NULL CHECK (
        typeof(evaluation_identity) = 'blob' AND length(evaluation_identity) > 0
    ),
    PRIMARY KEY (state_id, logical_slot),
    FOREIGN KEY (state_id) REFERENCES state(id) ON DELETE CASCADE
);
