use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub fact_id: Uuid,
    pub subject_entity_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    pub confidence: f32,
    pub is_current: bool,
}

impl Fact {
    pub fn builder(subject: String, predicate: String, object: String) -> FactBuilder {
        FactBuilder::new(subject, predicate, object)
    }

    pub fn was_valid_at(&self, date: DateTime<Utc>) -> bool {
        self.valid_from <= date && self.valid_to.is_none_or(|end| end > date)
    }
}

pub struct FactBuilder {
    subject_entity_id: Option<Uuid>,
    project_id: Option<Uuid>,
    subject: String,
    predicate: String,
    object: String,
    valid_from: DateTime<Utc>,
    valid_to: Option<DateTime<Utc>>,
    confidence: f32,
}

impl FactBuilder {
    fn new(subject: String, predicate: String, object: String) -> Self {
        Self {
            subject_entity_id: None,
            project_id: None,
            subject,
            predicate,
            object,
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
        }
    }

    pub fn subject_entity_id(mut self, id: Uuid) -> Self {
        self.subject_entity_id = Some(id);
        self
    }

    pub fn project_id(mut self, id: Option<Uuid>) -> Self {
        self.project_id = id;
        self
    }

    pub fn valid_from(mut self, date: DateTime<Utc>) -> Self {
        self.valid_from = date;
        self
    }

    pub fn valid_to(mut self, date: DateTime<Utc>) -> Self {
        self.valid_to = Some(date);
        self
    }

    pub fn confidence(mut self, score: f32) -> Self {
        self.confidence = score.clamp(0.0, 1.0);
        self
    }

    pub fn build(self) -> Fact {
        Fact {
            fact_id: Uuid::new_v4(),
            subject_entity_id: self.subject_entity_id,
            project_id: self.project_id,
            subject: self.subject,
            predicate: self.predicate,
            object: self.object,
            valid_from: self.valid_from,
            valid_to: self.valid_to,
            observed_at: Utc::now(),
            confidence: self.confidence,
            is_current: true,
        }
    }
}

pub async fn append_observation_fact(
    pool: &PgPool,
    entity_id: uuid::Uuid,
    entity_name: &str,
    obs_type: &str,
    content: &str,
    project_id: Option<uuid::Uuid>,
    confidence: f32,
) {
    if !crate::core::bitemporal_enabled() {
        return;
    }
    let fact = Fact::builder(
        entity_name.to_string(),
        obs_type.to_string(),
        content.to_string(),
    )
    .subject_entity_id(entity_id)
    .project_id(project_id)
    .confidence(confidence)
    .build();
    if let Err(e) = append_fact(pool, &fact).await {
        tracing::warn!(error = %e, "bitemporal append_fact skipped (table may be absent)");
    }
}

pub async fn append_fact(pool: &PgPool, fact: &Fact) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO brain_facts (
            fact_id, subject_entity_id, project_id, subject, predicate, object,
            valid_from, valid_to, observed_at, confidence, is_current
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)"#,
    )
    .bind(fact.fact_id)
    .bind(fact.subject_entity_id)
    .bind(fact.project_id)
    .bind(&fact.subject)
    .bind(&fact.predicate)
    .bind(&fact.object)
    .bind(fact.valid_from)
    .bind(fact.valid_to)
    .bind(fact.observed_at)
    .bind(fact.confidence)
    .bind(fact.is_current)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validity_window() {
        let now = Utc::now();
        let fact = Fact::builder("A".into(), "es".into(), "B".into())
            .valid_from(now - chrono::Duration::days(1))
            .valid_to(now + chrono::Duration::days(1))
            .build();
        assert!(fact.was_valid_at(now));
        assert!(!fact.was_valid_at(now - chrono::Duration::days(2)));
    }
}
