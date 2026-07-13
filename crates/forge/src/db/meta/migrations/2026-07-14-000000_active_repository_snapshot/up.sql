-- SPDX-FileCopyrightText: 2026 AerynOS Developers
-- SPDX-License-Identifier: MPL-2.0

-- A repository database has at most one accepted index snapshot. The fixed
-- key makes the singleton invariant enforceable even for callers outside the
-- typed Rust API.
CREATE TABLE active_repository_snapshot (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (
        typeof(singleton) = 'integer' AND singleton = 1
    ),
    index_uri TEXT NOT NULL CHECK (
        typeof(index_uri) = 'text' AND
        length(CAST(index_uri AS BLOB)) BETWEEN 1 AND 8192
    ),
    sha256 TEXT NOT NULL CHECK (
        typeof(sha256) = 'text' AND
        length(sha256) = 64 AND
        sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    byte_size BIGINT NOT NULL CHECK (
        typeof(byte_size) = 'integer' AND
        byte_size BETWEEN 0 AND 16777216
    )
);
