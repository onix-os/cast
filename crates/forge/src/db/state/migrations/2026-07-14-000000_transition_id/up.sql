-- SPDX-FileCopyrightText: 2026 AerynOS Developers
-- SPDX-License-Identifier: MPL-2.0

ALTER TABLE state ADD COLUMN transition_id TEXT NULL
    CHECK (
        transition_id IS NULL
        OR (
            typeof(transition_id) = 'text'
            AND length(transition_id) = 32
            AND length(CAST(transition_id AS BLOB)) = 32
            AND transition_id NOT GLOB '*[^0-9a-f]*'
        )
    );

CREATE UNIQUE INDEX state_transition_id_unique
    ON state (transition_id)
    WHERE transition_id IS NOT NULL;
