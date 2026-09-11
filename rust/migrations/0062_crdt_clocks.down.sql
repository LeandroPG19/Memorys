ALTER TABLE brain_observations DROP COLUMN IF EXISTS crdt_actor;
ALTER TABLE brain_observations DROP COLUMN IF EXISTS crdt_counter;
ALTER TABLE brain_entities DROP COLUMN IF EXISTS crdt_actor;
ALTER TABLE brain_entities DROP COLUMN IF EXISTS crdt_counter;
ALTER TABLE brain_relations DROP COLUMN IF EXISTS crdt_actor;
ALTER TABLE brain_relations DROP COLUMN IF EXISTS crdt_counter;
