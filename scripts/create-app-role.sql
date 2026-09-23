-- Bug 0.7 fix: run the MCP as a NON-superuser so RLS and the append-only audit
-- trigger are actually enforced. A superuser has implicit BYPASSRLS and is an
-- implicit MEMBER of every role (so pg_has_role(super,'cuba_admin') is TRUE),
-- which makes migration 0017 RLS policies and the 0016 audit guard inert.
--
-- This role owns nothing and is NOSUPERUSER NOBYPASSRLS, so:
--   * RLS tenant_isolation policies apply (project scoping is real).
--   * brain_audit_log UPDATE/DELETE is refused (not a cuba_admin member).
--
-- Idempotent. Run as a superuser (cuba) against each brain DB. The binary does
-- it for you, with the password the daemon already reads from pgpass_app
-- (~/.cache/memory-industry/pgpass_app):
--   memory-industry secure       (this same file, embedded in the binary)
--
-- By hand, name the role and hand the password over as session settings,
-- never pasted into this file. psql interpolates :'pw' in a script read from
-- stdin, not in -c:
--   { echo "SET memory_industry.app_role = 'cuba_app';"; \
--     echo "SET memory_industry.app_password = :'pw';"; cat scripts/create-app-role.sql; } \
--     | psql "$DATABASE_URL" -v pw="$(cat ~/.cache/memory-industry/pgpass_app)"
--
-- Until 0.27 this file created the role with a literal password written right
-- here, so every fresh install had a LOGIN role anyone who had read the repo
-- could open. A role that already exists keeps its password: the daemon of an
-- install that upgrades is connecting with it.
--
-- memory_industry.app_role names the role, and there is no default: until
-- 0.27 an unset one meant cuba_app, so a caller that forgot to name the role
-- altered the one the real daemon logs in as instead of failing. The tests set
-- it to a throwaway name because a role belongs to the whole server. Both
-- settings reach SQL only through format() with %I and %L.
--
-- Then point the app's DATABASE_URL at cuba_app instead of cuba.

DO $$
DECLARE
    app_role text := nullif(current_setting('memory_industry.app_role', true), '');
    app_password text := nullif(current_setting('memory_industry.app_password', true), '');
BEGIN
    IF app_role IS NULL THEN
        RAISE EXCEPTION 'memory_industry.app_role is not set, so there is no role to create or alter'
            USING HINT = 'SET memory_industry.app_role first (see the top of this file); there is no default role';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = app_role) THEN
        IF app_password IS NULL THEN
            RAISE EXCEPTION 'memory_industry.app_password is not set, so % cannot be created', app_role
                USING HINT = 'SET memory_industry.app_password first (see the top of this file); there is no default password';
        END IF;
        EXECUTE format(
            'CREATE ROLE %I LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS',
            app_role, app_password);
    ELSE
        EXECUTE format(
            'ALTER ROLE %I NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS', app_role);
    END IF;

    -- Schema + table privileges (data-plane only; no DDL, no ownership).
    EXECUTE format('GRANT USAGE ON SCHEMA public TO %I', app_role);
    EXECUTE format(
        'GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO %I', app_role);
    EXECUTE format('GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO %I', app_role);
    EXECUTE format('GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO %I', app_role);

    -- Future objects created by cuba inherit the same grants.
    EXECUTE format(
        'ALTER DEFAULT PRIVILEGES FOR ROLE cuba IN SCHEMA public '
        || 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', app_role);
    EXECUTE format(
        'ALTER DEFAULT PRIVILEGES FOR ROLE cuba IN SCHEMA public '
        || 'GRANT USAGE, SELECT ON SEQUENCES TO %I', app_role);
    EXECUTE format(
        'ALTER DEFAULT PRIVILEGES FOR ROLE cuba IN SCHEMA public '
        || 'GRANT EXECUTE ON FUNCTIONS TO %I', app_role);
END $$;
