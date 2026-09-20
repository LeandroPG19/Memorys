DROP POLICY IF EXISTS session_write ON brain_sessions;
DROP POLICY IF EXISTS session_insert ON brain_sessions;
DROP POLICY IF EXISTS session_update ON brain_sessions;
DROP POLICY IF EXISTS session_delete ON brain_sessions;
DROP POLICY IF EXISTS session_read ON brain_sessions;
DROP POLICY IF EXISTS tenant_isolation ON brain_sessions;

CREATE POLICY tenant_isolation ON brain_sessions
    USING (
        current_setting('app.current_project', true) IS NULL
     OR current_setting('app.current_project', true) = ''
     OR current_setting('app.current_project', true) = '*'
     OR project_id IS NULL
     OR project_id::text = current_setting('app.current_project', true)
    );
