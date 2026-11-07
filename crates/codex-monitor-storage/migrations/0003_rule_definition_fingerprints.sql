-- Immutable semantic identity for each persisted built-in rule version.
-- User-controlled enablement and notification settings remain in `rules`.

CREATE TABLE rule_definition_fingerprints (
    rule_id TEXT NOT NULL,
    rule_version INTEGER NOT NULL CHECK (rule_version > 0),
    semantic_sha256 BLOB NOT NULL CHECK (length(semantic_sha256) = 32),
    PRIMARY KEY (rule_id, rule_version),
    FOREIGN KEY (rule_id, rule_version)
        REFERENCES rules (rule_id, rule_version) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- Databases created by the pre-fingerprint v1 development schema may already
-- contain this rule. The digest is pinned by the core-domain test vector.
INSERT INTO rule_definition_fingerprints (
    rule_id,
    rule_version,
    semantic_sha256
)
SELECT
    rule_id,
    rule_version,
    X'4e04faf928b16996505cf36077b84e794dcd5b62d2924a77d1bc967336fc6a9f'
FROM rules
WHERE rule_id = 'suspected-reasoning-limit-pattern'
  AND rule_version = 1;
