-- SPDX-FileCopyrightText: 2026 AerynOS Developers
-- SPDX-License-Identifier: MPL-2.0

DROP INDEX state_transition_id_unique;
ALTER TABLE state DROP COLUMN transition_id;
