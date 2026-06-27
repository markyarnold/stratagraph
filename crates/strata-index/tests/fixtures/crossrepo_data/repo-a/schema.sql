-- A distilled Postgres schema: a small multi-table FK graph plus a cumulative
-- ALTER. Hand-written for the data-plane fixture; not copied from any client.

CREATE TABLE orgs (
    id BIGINT PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE TABLE users (
    id BIGINT PRIMARY KEY,
    email VARCHAR(255) NOT NULL,
    org_id BIGINT NOT NULL REFERENCES orgs(id)
);

CREATE TABLE memberships (
    user_id BIGINT NOT NULL,
    org_id BIGINT NOT NULL,
    role TEXT,
    PRIMARY KEY (user_id, org_id),
    FOREIGN KEY (user_id) REFERENCES users(id),
    CONSTRAINT fk_org FOREIGN KEY (org_id) REFERENCES orgs(id)
);

-- A non-table statement interleaved: skipped, not errored.
CREATE INDEX idx_users_email ON users (email);

-- A cumulative ALTER: adds a column to the already-declared users table.
ALTER TABLE users ADD COLUMN last_login TIMESTAMPTZ;
