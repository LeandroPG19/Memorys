use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 2;

pub const MAX_EMBEDDING_DIM: usize = 16_000;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub schema_version: u32,
    pub manifest_hash: String,
    pub project_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub exported_at: DateTime<Utc>,
    pub counts: Counts,
    pub with_embeddings: bool,
    #[serde(default)]
    pub embedding_dim: Option<usize>,
    #[serde(default)]
    pub node_id: Option<Uuid>,
    #[serde(default)]
    pub embedding_model: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Counts {
    pub entities: u32,
    pub observations: u32,
    pub episodes: u32,
    pub decisions: u32,
    pub errors: u32,
    pub relations: u32,
    #[serde(default)]
    pub artifacts: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectRow {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EntityFile {
    pub id: Uuid,
    pub name: String,
    pub entity_type: String,
    pub importance: f64,
    pub access_count: i32,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub observations: Vec<ObservationRow>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ObservationRow {
    pub id: Uuid,
    pub content: String,
    pub observation_type: String,
    pub source: String,
    pub importance: f64,
    pub tags: Vec<String>,
    pub project_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub embedding_model: Option<String>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub version: Option<i32>,
    #[serde(default)]
    pub previous_versions: Option<Value>,
    #[serde(default)]
    pub origin_node: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub verified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub trust: Option<String>,
    #[serde(default)]
    pub crdt_actor: Option<String>,
    #[serde(default)]
    pub crdt_counter: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EpisodeFile {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub content: String,
    pub actors: Vec<String>,
    pub artifacts: Vec<String>,
    pub importance: f64,
    pub project_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ErrorFile {
    pub id: Uuid,
    pub error_type: String,
    pub error_message: String,
    pub solution: Option<String>,
    pub resolved: bool,
    pub project: String,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RelationRow {
    pub id: Uuid,
    pub from_entity: Uuid,
    pub to_entity: Uuid,
    pub relation_type: String,
    pub strength: f64,
    pub bidirectional: bool,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    #[serde(default = "default_provenance")]
    pub provenance: String,
}

fn default_provenance() -> String {
    "extracted".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FactRow {
    pub fact_id: Uuid,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub subject_entity_id: Option<Uuid>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub is_current: Option<bool>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub layer_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, sqlx::FromRow)]
pub struct ProcedureRow {
    pub id: Uuid,
    pub name: String,
    pub steps: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub trigger_context: String,
    #[serde(default)]
    pub preconditions: String,
    #[serde(default)]
    pub verification: String,
    #[serde(default)]
    pub success_count: i32,
    #[serde(default)]
    pub failure_count: i32,
    #[serde(default)]
    pub last_outcome: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub embedding_model: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, sqlx::FromRow)]
pub struct SourceTrustRow {
    pub source: String,
    pub alpha: f64,
    pub beta: f64,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactRow {
    pub id: Uuid,
    pub path: String,
    pub content: String,
    pub content_hash: String,
    pub version: i64,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub origin_node: Option<String>,
    #[serde(default)]
    pub crdt_actor: Option<String>,
    #[serde(default)]
    pub crdt_counter: i64,
    pub updated_at: DateTime<Utc>,
}

pub fn payload_hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

pub fn payload_hash(s: &str) -> String {
    payload_hash_bytes(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_sha256_and_deterministic() {
        assert_eq!(
            payload_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(payload_hash("brain"), payload_hash("brain"));
        assert_ne!(payload_hash("a"), payload_hash("b"));
        assert_eq!(payload_hash("x").len(), 64);
    }

    #[test]
    fn relation_row_round_trips_provenance_through_json() {
        let rel = RelationRow {
            id: Uuid::new_v4(),
            from_entity: Uuid::new_v4(),
            to_entity: Uuid::new_v4(),
            relation_type: "related_to".to_string(),
            strength: 0.42,
            bidirectional: false,
            project_id: None,
            created_at: Utc::now(),
            provenance: "predicted".to_string(),
        };

        let json = serde_json::to_string(&rel).unwrap();
        assert!(
            json.contains("\"provenance\":\"predicted\""),
            "exported relations.json must carry provenance, not silently drop it: {json}"
        );

        let round_tripped: RelationRow = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.provenance, "predicted");
    }

    #[test]
    fn relation_row_defaults_provenance_for_pre_migration_json() {
        let legacy_json = serde_json::json!({
            "id": Uuid::new_v4(),
            "from_entity": Uuid::new_v4(),
            "to_entity": Uuid::new_v4(),
            "relation_type": "related_to",
            "strength": 0.5,
            "bidirectional": false,
            "project_id": null,
            "created_at": Utc::now(),
        });

        let rel: RelationRow = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(rel.provenance, "extracted");
    }

    fn a_fact() -> FactRow {
        FactRow {
            fact_id: Uuid::new_v4(),
            subject: "cuba-memorys".to_string(),
            predicate: "runs_on".to_string(),
            object: "postgres 18".to_string(),
            valid_from: Utc::now(),
            observed_at: Utc::now(),
            valid_to: None,
            subject_entity_id: Some(Uuid::new_v4()),
            project_id: None,
            confidence: Some(0.9),
            is_current: Some(true),
            created_at: Some(Utc::now()),
            layer_name: Some("episodic".to_string()),
        }
    }

    #[test]
    fn a_fact_travels_by_layer_name_because_the_layer_uuid_is_local_to_one_install() {
        let json = serde_json::to_value(a_fact()).expect("serialise");
        let keys: Vec<&String> = json.as_object().expect("an object").keys().collect();

        assert!(
            !keys.iter().any(|k| k.as_str() == "layer_id"),
            "brain_memory_layers.layer_id is gen_random_uuid() and migration 0020 does not pin \
             it, so 0 of 4 layer ids match between two installs (measured). A bundle carrying \
             layer_id raises 23503 on import, and that aborts the whole transaction, not one \
             row. Keys exported: {keys:?}"
        );
        assert_eq!(
            json["layer_name"], "episodic",
            "the name is the only part of a layer that means the same thing on both machines"
        );

        let back: FactRow = serde_json::from_value(json).expect("round trip");
        assert_eq!(back.layer_name.as_deref(), Some("episodic"));
    }

    #[test]
    fn a_fact_with_no_layer_still_parses_because_that_is_every_fact_in_the_live_corpus() {
        let json = serde_json::json!({
            "fact_id": Uuid::new_v4(),
            "subject": "s",
            "predicate": "p",
            "object": "o",
            "valid_from": Utc::now(),
            "observed_at": Utc::now(),
        });

        let fact: FactRow = serde_json::from_value(json).expect(
            "all 990 facts in the live corpus have layer_id NULL, and most columns of \
                     brain_facts are nullable or defaulted — a bundle that omits them is not a \
                     corrupt bundle",
        );
        assert_eq!(fact.layer_name, None);
        assert_eq!(fact.confidence, None);
        assert_eq!(fact.is_current, None);
    }

    #[test]
    fn a_procedure_leaves_its_embedding_behind_and_takes_only_the_model_name() {
        let proc = ProcedureRow {
            id: Uuid::new_v4(),
            name: "levantar el entorno".to_string(),
            steps: serde_json::json!([{"do": "docker compose up"}]),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            trigger_context: "cuando hay que levantar los servicios".to_string(),
            preconditions: String::new(),
            verification: "el healthcheck en verde".to_string(),
            success_count: 3,
            failure_count: 1,
            last_outcome: Some("success".to_string()),
            last_used_at: Some(Utc::now()),
            project_id: None,
            embedding_model: Some("bge-m3".to_string()),
        };

        let json = serde_json::to_value(&proc).expect("serialise");
        assert!(
            json.get("embedding").is_none(),
            "vectors travel out of band in embeddings.bin.zst, not inside the row: a 1024-d \
             halfvec inlined per procedure is bulk the receiver has to re-embed anyway when the \
             model differs. Got: {json}"
        );
        assert_eq!(json["embedding_model"], "bge-m3");

        let back: ProcedureRow = serde_json::from_value(json).expect("round trip");
        assert_eq!(
            back.success_count, 3,
            "a track record is the point of a procedure"
        );
        assert_eq!(back.failure_count, 1);
    }

    #[test]
    fn a_procedure_from_an_older_bundle_keeps_its_counters_at_zero_instead_of_failing() {
        let json = serde_json::json!({
            "id": Uuid::new_v4(),
            "name": "receta mínima",
            "steps": [],
            "created_at": Utc::now(),
            "updated_at": Utc::now(),
        });

        let proc: ProcedureRow = serde_json::from_value(json).expect("parse");
        assert_eq!(proc.success_count, 0);
        assert_eq!(proc.failure_count, 0);
        assert_eq!(
            proc.trigger_context, "",
            "the column is NOT NULL DEFAULT '' in the database, so the empty string is the \
             value the row would have had anyway"
        );
    }

    #[test]
    fn source_trust_carries_both_halves_of_the_posterior() {
        let json = serde_json::json!({
            "source": "inference",
            "alpha": 7.0,
            "beta": 2.0,
        });

        let trust: SourceTrustRow = serde_json::from_value(json).expect("parse");
        assert_eq!(trust.alpha, 7.0);
        assert_eq!(
            trust.beta, 2.0,
            "alpha alone is a success count, not a posterior — a source with 7 successes and 2 \
             failures is not the same source as one with 7 and 0, and merging on alpha only \
             would make every source look perfect"
        );
        assert_eq!(trust.updated_at, None);
    }

    async fn test_pool() -> sqlx::PgPool {
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");
        crate::db::create_pool(&url)
            .await
            .expect("connect to test database")
    }

    fn unique_name(prefix: &str) -> String {
        format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
    }

    #[tokio::test]
    #[ignore]
    async fn a_layer_resolves_by_name_because_its_uuid_belongs_to_one_installation() {
        let pool = test_pool().await;

        for name in ["episodic", "semantic", "working", "project"] {
            let resolved: Option<Uuid> = sqlx::query_scalar("SELECT brain_layer_by_name($1)")
                .bind(name)
                .fetch_one(&pool)
                .await
                .expect("the lookup must not raise for a name that exists");
            let local: Uuid = sqlx::query_scalar(
                "SELECT layer_id FROM brain_memory_layers WHERE layer_name::text = $1",
            )
            .bind(name)
            .fetch_one(&pool)
            .await
            .expect("migration 0020 seeds all four layers");
            assert_eq!(
                resolved,
                Some(local),
                "layer {name} has to resolve to the id this database generated, which is the \
                 whole point: the sender's id for it is a different uuid"
            );
        }

        let unknown: Option<Uuid> = sqlx::query_scalar("SELECT brain_layer_by_name($1)")
            .bind("a_layer_from_some_future_version")
            .fetch_one(&pool)
            .await
            .expect(
                "an unknown layer name must come back NULL. Casting it to memory_layer_type \
                 instead raises 22P02, and inside the import transaction that is not one bad \
                 fact, it is every fact in the bundle",
            );
        assert_eq!(unknown, None);

        let subject = unique_name("fact_subject");
        let foreign_layer = Uuid::new_v4();
        let rejected = sqlx::query(
            "INSERT INTO brain_facts (subject, predicate, object, layer_id) \
             VALUES ($1, 'travels_as', 'a raw uuid', $2)",
        )
        .bind(&subject)
        .bind(foreign_layer)
        .execute(&pool)
        .await;
        assert!(
            rejected.is_err(),
            "this is the import killer, restated: another machine's layer_id is not present \
             here, so the FK rejects it — measured 0 of 4 ids shared between two installs"
        );

        let fact_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO brain_facts (fact_id, subject, predicate, object, layer_id) \
             VALUES ($1, $2, 'travels_as', 'a layer name', brain_layer_by_name('episodic'))",
        )
        .bind(fact_id)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("the same fact, carrying the name instead of the uuid, has to land");

        let stored: Option<Uuid> =
            sqlx::query_scalar("SELECT layer_id FROM brain_facts WHERE fact_id = $1")
                .bind(fact_id)
                .fetch_one(&pool)
                .await
                .expect("read the fact back");
        assert_ne!(
            stored,
            Some(foreign_layer),
            "the imported fact must point at the local layer, not at the sender's uuid"
        );
        assert!(
            stored.is_some(),
            "and it must not have lost its layer either"
        );

        sqlx::query("DELETE FROM brain_facts WHERE fact_id = $1")
            .bind(fact_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM brain_tombstones WHERE row_id = $1")
            .bind(fact_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    #[ignore]
    async fn a_deleted_fact_leaves_a_tombstone_even_though_its_key_is_not_called_id() {
        let pool = test_pool().await;

        let fact_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO brain_facts (fact_id, subject, predicate, object) \
             VALUES ($1, $2, 'is', 'about to be deleted')",
        )
        .bind(fact_id)
        .bind(unique_name("doomed_fact"))
        .execute(&pool)
        .await
        .expect("seed the fact the peer deletes");

        let procedure_id = Uuid::new_v4();
        sqlx::query("INSERT INTO brain_procedures (id, name) VALUES ($1, $2)")
            .bind(procedure_id)
            .bind(unique_name("doomed_procedure"))
            .execute(&pool)
            .await
            .expect("seed the procedure the peer deletes");

        let entity_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')",
        )
        .bind(entity_id)
        .bind(unique_name("doomed_entity"))
        .execute(&pool)
        .await
        .expect("seed a row from one of the six tables 0045 already covered");

        sqlx::query("DELETE FROM brain_facts WHERE fact_id = $1")
            .bind(fact_id)
            .execute(&pool)
            .await
            .expect(
                "with the 0045 body the trigger reads OLD.id, brain_facts has no such column, \
                 and the DELETE itself fails with 42703 — a row that cannot be deleted at all",
            );
        sqlx::query("DELETE FROM brain_procedures WHERE id = $1")
            .bind(procedure_id)
            .execute(&pool)
            .await
            .expect("delete the procedure");
        sqlx::query("DELETE FROM brain_entities WHERE id = $1")
            .bind(entity_id)
            .execute(&pool)
            .await
            .expect("delete the entity");

        for (table, row_id) in [
            ("brain_facts", fact_id),
            ("brain_procedures", procedure_id),
            ("brain_entities", entity_id),
        ] {
            let recorded: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM brain_tombstones WHERE table_name = $1 AND row_id = $2",
            )
            .bind(table)
            .bind(row_id)
            .fetch_one(&pool)
            .await
            .expect("count the tombstones");
            assert_eq!(
                recorded, 1,
                "a delete on {table} that leaves no tombstone is a delete that the peer undoes \
                 on the next round. brain_entities is in this list on purpose: generalising \
                 brain_record_tombstone() replaces the body the six triggers from 0045 already \
                 run, so it has to keep working for them too"
            );
        }

        for row_id in [fact_id, procedure_id, entity_id] {
            sqlx::query("DELETE FROM brain_tombstones WHERE row_id = $1")
                .bind(row_id)
                .execute(&pool)
                .await
                .ok();
        }
    }
}
