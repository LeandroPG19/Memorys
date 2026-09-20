-- Entity names were globally unique. Every project's architecture_decisions
-- (and any other shared label) therefore hung off one node, and ON CONFLICT
-- (name) reused the first writer's row. Procedures already use
-- UNIQUE (name, project_id). Entities follow.
--
-- NULLS NOT DISTINCT: one (name, NULL) row remains, which is the unscoped
-- leftover from installs that never bound a project.

ALTER TABLE brain_entities DROP CONSTRAINT IF EXISTS brain_entities_name_key;

ALTER TABLE brain_entities
    ADD CONSTRAINT uq_brain_entities_name_project
    UNIQUE NULLS NOT DISTINCT (name, project_id);

-- Clone the shared architecture_decisions node per project that already wrote
-- decisions against it, then re-point those observations.
INSERT INTO brain_entities (name, entity_type, project_id)
SELECT 'architecture_decisions', 'concept', o.project_id
FROM brain_observations o
JOIN brain_entities e ON e.id = o.entity_id
WHERE e.name = 'architecture_decisions'
  AND o.observation_type = 'decision'
  AND o.project_id IS NOT NULL
GROUP BY o.project_id
ON CONFLICT ON CONSTRAINT uq_brain_entities_name_project DO NOTHING;

UPDATE brain_observations o
SET entity_id = e_new.id
FROM brain_entities e_old, brain_entities e_new
WHERE o.entity_id = e_old.id
  AND e_old.name = 'architecture_decisions'
  AND e_new.name = 'architecture_decisions'
  AND e_new.project_id IS NOT DISTINCT FROM o.project_id
  AND e_old.id IS DISTINCT FROM e_new.id
  AND o.observation_type = 'decision';
