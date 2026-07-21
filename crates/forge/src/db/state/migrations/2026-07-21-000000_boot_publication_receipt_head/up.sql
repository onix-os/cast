CREATE TABLE boot_publication_receipt_head (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (
        typeof(singleton) = 'integer'
        AND singleton = 1
    ),
    committed_receipt_sha256 BLOB NULL CHECK (
        committed_receipt_sha256 IS NULL
        OR (
            typeof(committed_receipt_sha256) = 'blob'
            AND length(committed_receipt_sha256) = 32
        )
    ),
    pending_transition_id TEXT NULL,
    pending_receipt_sha256 BLOB NULL,
    CHECK (
        (
            pending_transition_id IS NULL
            AND pending_receipt_sha256 IS NULL
        )
        OR (
            typeof(pending_transition_id) = 'text'
            AND length(pending_transition_id) = 32
            AND length(CAST(pending_transition_id AS BLOB)) = 32
            AND pending_transition_id NOT GLOB '*[^0-9a-f]*'
            AND typeof(pending_receipt_sha256) = 'blob'
            AND length(pending_receipt_sha256) = 32
        )
    )
);

INSERT INTO boot_publication_receipt_head (
    singleton,
    committed_receipt_sha256,
    pending_transition_id,
    pending_receipt_sha256
) VALUES (1, NULL, NULL, NULL);
