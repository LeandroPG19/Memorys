-- Bug 0.7 fix: run the MCP as a NON-superuser so RLS and the append-only audit
-- trigger are actually enforced. A superuser has implicit BYPASSRLS and is an
-- implicit MEMBER of every role (so pg_has_role(super,'cuba_admin') is TRUE),
-- which makes migration 0017 RLS policies and the 0016 audit guard inert.
--
-- This role owns nothing and is NOSUPERUSER NOBYPASSRLS, so:
--   * RLS tenant_isolation policies apply (project scoping is real).
--   * brain_audit_log UPDATE/DELETE is refused (not a cuba_admin member).
--
-- Idempotent. Run as a superuser (cuba) against each brain DB:
--   psql "$DATABASE_URL" -f scripts/create-app-role.sql
--   cuba-memorys secure          (same file, embedded in the binary)
--
-- Then point the app's DATABASE_URL at cuba_app instead of cuba.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'cuba_app') THEN
        CREATE ROLE cuba_app LOGIN PASSWORD 'app2026'
            NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;
    ELSE
        ALTER ROLE cuba_app NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;
    END IF;
END $$;

-- Schema + table privileges (data-plane only; no DDL, no ownership).
GRANT USAGE ON SCHEMA public TO cuba_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO cuba_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO cuba_app;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO cuba_app;

-- Future objects created by cuba inherit the same grants.
ALTER DEFAULT PRIVILEGES FOR ROLE cuba IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO cuba_app;
ALTER DEFAULT PRIVILEGES FOR ROLE cuba IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO cuba_app;
ALTER DEFAULT PRIVILEGES FOR ROLE cuba IN SCHEMA public
    GRANT EXECUTE ON FUNCTIONS TO cuba_app;
