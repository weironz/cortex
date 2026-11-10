CREATE TABLE ledger_meta (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    version INTEGER NOT NULL CHECK(version = 1)
) STRICT;
CREATE TABLE intents (
    namespace BLOB NOT NULL CHECK(length(namespace) = 32),
    op_id BLOB NOT NULL CHECK(length(op_id) = 16),
    approval_id BLOB NOT NULL CHECK(length(approval_id) = 16),
    resource_digest BLOB NOT NULL CHECK(length(resource_digest) = 32),
    action_digest BLOB NOT NULL CHECK(length(action_digest) = 32),
    state TEXT NOT NULL CHECK(state IN ('prepared','verified','unknown','timed_out','cancelled')),
    PRIMARY KEY(namespace, op_id),
    UNIQUE(namespace, op_id, resource_digest)
) STRICT;
CREATE TABLE fences (
    resource_digest BLOB PRIMARY KEY NOT NULL CHECK(length(resource_digest) = 32),
    namespace BLOB NOT NULL CHECK(length(namespace) = 32),
    op_id BLOB NOT NULL CHECK(length(op_id) = 16),
    UNIQUE(namespace, op_id),
    FOREIGN KEY(namespace, op_id, resource_digest)
        REFERENCES intents(namespace, op_id, resource_digest)
) STRICT;
INSERT INTO ledger_meta(singleton, version) VALUES(1, 1);
