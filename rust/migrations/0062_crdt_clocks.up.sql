-- CRDT clocks on graph rows: actor + monotonic counter per writer.

ALTER TABLE brain_observations
    ADD COLUMN IF NOT EXISTS crdt_actor text,
    ADD COLUMN IF NOT EXISTS crdt_counter bigint NOT NULL DEFAULT 1;

ALTER TABLE brain_entities
    ADD COLUMN IF NOT EXISTS crdt_actor text,
    ADD COLUMN IF NOT EXISTS crdt_counter bigint NOT NULL DEFAULT 1;

ALTER TABLE brain_relations
    ADD COLUMN IF NOT EXISTS crdt_actor text,
    ADD COLUMN IF NOT EXISTS crdt_counter bigint NOT NULL DEFAULT 1;

UPDATE brain_observations
SET crdt_actor = COALESCE(NULLIF(origin_node, ''), 'legacy'),
    crdt_counter = GREATEST(COALESCE(version, 1), 1)
WHERE crdt_actor IS NULL;

UPDATE brain_entities
SET crdt_actor = 'legacy',
    crdt_counter = 1
WHERE crdt_actor IS NULL;

UPDATE brain_relations
SET crdt_actor = 'legacy',
    crdt_counter = 1
WHERE crdt_actor IS NULL;

COMMENT ON COLUMN brain_observations.crdt_actor IS
    'Writer identity for CRDT merge (node id or MCP client id).';
COMMENT ON COLUMN brain_observations.crdt_counter IS
    'Lamport-style counter for this actor; bumps on semantic writes.';
