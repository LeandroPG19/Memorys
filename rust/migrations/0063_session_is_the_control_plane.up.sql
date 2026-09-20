-- brain_sessions is the control plane that *changes* the tenant. Migration 0017
-- put the same tenant_isolation policy on it as on observations. PostgreSQL
-- uses USING as WITH CHECK when none is given, so INSERT/UPDATE of a row whose
-- project_id differs from app.current_project (already stamped by the pool from
-- the previous jornada) is refused. Measured 2026-09-17 on the live daemon:
-- cuba_jornada start --project MemoryIndustry and cuba_proyecto switch both
-- failed RLS while a mapupita-proyectos-web session was still open.
--
-- SELECT stays scoped. Writes are allowed: the tenant of a session *is* its
-- project_id, not the setting left over from the session it replaces.

DROP POLICY IF EXISTS tenant_isolation ON brain_sessions;
DROP POLICY IF EXISTS session_read ON brain_sessions;
DROP POLICY IF EXISTS session_insert ON brain_sessions;
DROP POLICY IF EXISTS session_update ON brain_sessions;
DROP POLICY IF EXISTS session_delete ON brain_sessions;
DROP POLICY IF EXISTS session_write ON brain_sessions;

CREATE POLICY session_read ON brain_sessions
    FOR SELECT
    USING (
        current_setting('app.current_project', true) IS NULL
     OR current_setting('app.current_project', true) = ''
     OR current_setting('app.current_project', true) = '*'
     OR project_id IS NULL
     OR project_id::text = current_setting('app.current_project', true)
    );

-- FOR ALL would OR into SELECT and hide nothing. Writes only.
CREATE POLICY session_insert ON brain_sessions
    FOR INSERT
    WITH CHECK (true);

CREATE POLICY session_update ON brain_sessions
    FOR UPDATE
    USING (true)
    WITH CHECK (true);

CREATE POLICY session_delete ON brain_sessions
    FOR DELETE
    USING (true);
