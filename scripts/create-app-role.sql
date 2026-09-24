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
-- memory_industry.app_password carries the SCRAM-SHA-256 verifier of that
-- password, never the password: SCRAM-SHA-256$4096:<salt>$<StoredKey>:<ServerKey>,
-- the form pg_authid keeps. CREATE ROLE stores a verifier as it is handed
-- («If the presented password string is already in MD5-encrypted or
-- SCRAM-encrypted format, then it is stored as-is regardless of
-- password_encryption», PostgreSQL docs, CREATE ROLE), so the password itself
-- never reaches the server, where log_statement and pg_stat_statements keep
-- what they are sent. Anything else is refused before a role is created.
--
-- By hand, compute the verifier on the client and hand it over with the role
-- name as session settings, never pasted into this file. psql interpolates
-- :'verifier' in a script read from stdin, not in -c:
--   { echo "SET memory_industry.app_role = 'cuba_app';"; \
--     echo "SET memory_industry.app_password = :'verifier';"; cat scripts/create-app-role.sql; } \
--     | psql "$DATABASE_URL" -v verifier="$(python3 -c 'import base64,hashlib,hmac,os,sys;p=sys.stdin.read().strip().encode();s=os.urandom(16);k=hashlib.pbkdf2_hmac("sha256",p,s,4096);h=lambda m:hmac.new(k,m,"sha256").digest();b=lambda x:base64.b64encode(x).decode();print("SCRAM-SHA-256$4096:%s$%s:%s"%(b(s),b(hashlib.sha256(h(b"Client Key")).digest()),b(h(b"Server Key"))))' < ~/.cache/memory-industry/pgpass_app)"
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
    -- The value is not repeated in the error: when it is the password, the
    -- server log would keep it from there.
    IF app_password IS NOT NULL AND NOT starts_with(app_password, 'SCRAM-SHA-256$') THEN
        RAISE EXCEPTION 'memory_industry.app_password is not a SCRAM-SHA-256 verifier, so it is not handed to CREATE ROLE'
            USING HINT = 'SET it to the verifier of the password, never the password (see the top of this file)';
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
