mod common;

use common::in_a_scratch_database;
use memory_industry::search::calibrate::{
    CalibrationReport, DEFAULT_ALPHA, load_ood_threshold, store_ood_threshold,
};
use serde_json::json;
use sqlx::{Executor, PgPool};

/// The real migration, not a copy of it: the table this reads is the one the
/// daemon has, and a later migration that changes it changes this test too.
const CALIBRATION_TABLE: &str = include_str!("../migrations/0032_calibration.up.sql");

/// None of the four constants the mutation run put in place of the body
/// (None, 0.0, 1.0, -1.0), and exact in binary so it survives the round trip
/// through DOUBLE PRECISION bit for bit.
const CALIBRATED: f64 = 0.4375;
const CALIBRATED_DIM: usize = 384;
const OTHER_DIM: usize = 1024;

fn tag() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

async fn remember(pool: &PgPool, entity: &str, content: &str) {
    memory_industry::handlers::dispatch(
        pool,
        "cuba_cronica",
        json!({
            "action": "add",
            "entity_name": entity,
            "content": content,
            "observation_type": "fact",
            "source": "agent"
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("seeding {entity} through cuba_cronica: {e:#}"));
}

fn report_for(dim: usize) -> CalibrationReport {
    CalibrationReport {
        embedding_dim: dim,
        fit_samples: 0,
        theoretical_threshold: 0.0,
        corpus: None,
        queries: None,
        alpha: DEFAULT_ALPHA,
        conformal_threshold: Some(CALIBRATED),
        theoretical_rejects_corpus: 0.0,
    }
}

// A scratch database and not brain_gate: `ood_threshold` is a single row the
// search path reads, and a value left there by this test would change what
// every later faro test abstains on.
#[tokio::test]
async fn the_calibrated_threshold_comes_back_only_for_the_dimension_it_was_measured_on() {
    in_a_scratch_database("brain_oodcal", |url| async move {
        let pool = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database");
        pool.execute(CALIBRATION_TABLE)
            .await
            .expect("creating brain_calibration from its migration");

        let before = load_ood_threshold(&pool, CALIBRATED_DIM).await;
        store_ood_threshold(&pool, CALIBRATED, &report_for(CALIBRATED_DIM))
            .await
            .expect("storing the calibrated threshold");
        let same = load_ood_threshold(&pool, CALIBRATED_DIM).await;
        let other = load_ood_threshold(&pool, OTHER_DIM).await;
        pool.close().await;

        assert_eq!(
            before, None,
            "nothing was ever calibrated, so there is no threshold to hand back; a constant here \
             is a threshold nobody measured"
        );
        assert_eq!(
            same,
            Some(CALIBRATED),
            "the threshold calibrated for {CALIBRATED_DIM}-d has to come back exactly as stored \
             when the model is still {CALIBRATED_DIM}-d; otherwise the calibration is ignored"
        );
        assert_eq!(
            other, None,
            "a threshold measured on {CALIBRATED_DIM}-d vectors says nothing about a \
             {OTHER_DIM}-d model and must be dropped, not applied"
        );
    })
    .await;
}

// A migrated scratch database and not brain_gate: the export writes one note
// per entity in the whole base, and against the shared one two entities from
// other tests whose names differ only in case, or in a character the note name
// replaces, would land on one file on Windows and fail this for a reason that
// is not the one it is here to catch.
#[tokio::test]
async fn the_obsidian_export_reports_the_notes_it_actually_wrote() {
    in_a_scratch_database("brain_export", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let t = tag();
        let seeded = [
            (
                format!("exportada_uno_{t}"),
                format!("primera nota exportada {t}"),
            ),
            (
                format!("exportada_dos_{t}"),
                format!("segunda nota exportada {t}"),
            ),
        ];
        for (entity, content) in &seeded {
            remember(&pool, entity, content).await;
        }

        let dir = std::env::temp_dir().join(format!("cuba-obsidian-{t}"));
        let reported = memory_industry::export::export_obsidian(&pool, &dir).await;
        let notes: Vec<String> = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.ends_with(".md") && name != "README.md")
                    .collect()
            })
            .unwrap_or_default();
        let bodies: Vec<Option<String>> = seeded
            .iter()
            .map(|(entity, _)| std::fs::read_to_string(dir.join(format!("{entity}.md"))).ok())
            .collect();
        std::fs::remove_dir_all(&dir).ok();
        pool.close().await;

        let reported = reported.expect("the export failed on a base with two entities");
        assert_eq!(
            reported,
            notes.len(),
            "the export says it wrote {reported} notes and the directory holds {}: {notes:?}",
            notes.len()
        );
        for ((entity, content), body) in seeded.iter().zip(&bodies) {
            let body = body.as_deref().unwrap_or_else(|| {
                panic!("no note for {entity}, which was seeded; the directory holds {notes:?}")
            });
            assert!(
                body.contains(content.as_str()),
                "the note for {entity} does not carry its observation «{content}»:\n{body}"
            );
        }
    })
    .await;
}
