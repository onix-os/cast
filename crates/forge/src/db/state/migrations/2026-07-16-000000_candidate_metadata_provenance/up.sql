CREATE TABLE state_metadata_provenance (
    state_id INTEGER NOT NULL PRIMARY KEY,
    os_release_sha256 BLOB NOT NULL CHECK (
        typeof(os_release_sha256) = 'blob'
        AND length(os_release_sha256) = 32
    ),
    system_model_sha256 BLOB NOT NULL CHECK (
        typeof(system_model_sha256) = 'blob'
        AND length(system_model_sha256) = 32
    ),
    FOREIGN KEY(state_id) REFERENCES state(id) ON DELETE CASCADE
);
