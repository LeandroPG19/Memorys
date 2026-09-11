-- Shared artifacts between agents on the same daemon (and peers via bundles).
-- Optimistic concurrency via version; short leases via locked_by/lock_until.

CREATE TABLE IF NOT EXISTS brain_artifacts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id uuid REFERENCES brain_projects(id) ON DELETE SET NULL,
    path text NOT NULL,
    content text NOT NULL DEFAULT '',
    content_hash text NOT NULL,
    version bigint NOT NULL DEFAULT 1,
    locked_by text,
    lock_until timestamptz,
    origin_node text,
    crdt_actor text,
    crdt_counter bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT NOW(),
    updated_at timestamptz NOT NULL DEFAULT NOW(),
    UNIQUE (project_id, path)
);

CREATE UNIQUE INDEX IF NOT EXISTS brain_artifacts_null_project_path
    ON brain_artifacts (path) WHERE project_id IS NULL;

CREATE INDEX IF NOT EXISTS brain_artifacts_project_path
    ON brain_artifacts (project_id, path);

CREATE INDEX IF NOT EXISTS brain_artifacts_updated
    ON brain_artifacts (updated_at DESC);

ALTER TABLE brain_artifacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE brain_artifacts FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON brain_artifacts;
CREATE POLICY tenant_isolation ON brain_artifacts
    USING (
        current_setting('app.current_project', true) IS NULL
     OR current_setting('app.current_project', true) = ''
     OR current_setting('app.current_project', true) = '*'
     OR project_id IS NULL
     OR project_id::text = current_setting('app.current_project', true)
    );

COMMENT ON TABLE brain_artifacts IS
    'Versioned shared files/notes for multi-agent coordination. put requires base_version; locks are leases.';
