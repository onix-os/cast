CREATE TABLE boot_publication_receipts (
    receipt_sha256 BLOB NOT NULL PRIMARY KEY CHECK (
        typeof(receipt_sha256) = 'blob'
        AND length(receipt_sha256) = 32
    ),
    transition_id TEXT NOT NULL CHECK (
        typeof(transition_id) = 'text'
        AND length(transition_id) = 32
        AND length(CAST(transition_id AS BLOB)) = 32
        AND transition_id NOT GLOB '*[^0-9a-f]*'
    ) UNIQUE,
    canonical_body BLOB NOT NULL CHECK (
        typeof(canonical_body) = 'blob'
        AND length(canonical_body) > 0
        AND length(canonical_body) <= 16777216
    )
);
