-- Cannot restore a global UNIQUE(name) if two projects now share a name.
-- Drop the composite constraint only; a later cleanup can collapse names.

ALTER TABLE brain_entities DROP CONSTRAINT IF EXISTS uq_brain_entities_name_project;
