//! Query-class router (LightRAG dual-level / GraphRAG local vs global).
//! Classification is lexical and deterministic — no LLM.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryClass {
    Factoid,
    MultiHop,
    Global,
}

pub fn classify_query(q: &str) -> QueryClass {
    let s = q.to_lowercase();
    const GLOBAL: &[&str] = &[
        "temas principales",
        "main themes",
        "resumen del corpus",
        "overview of",
        "comunidades",
        "communities",
        "qué cubre",
        "what are the themes",
    ];
    if GLOBAL.iter().any(|k| s.contains(k)) {
        return QueryClass::Global;
    }
    const HOP: &[&str] = &[
        "cómo se relaciona",
        "como se relaciona",
        "how does",
        "how is",
        "relaciona",
        "relacion",
        "relación",
        "depende de",
        "depende",
        "depends on",
        "conecta",
        "que une",
        "ligado",
        "por qué",
        "por que",
        "why does",
        "cadena",
        "multi-hop",
        "camino entre",
        "path between",
        "causas de",
        "causes of",
    ];
    if HOP.iter().any(|k| s.contains(k)) {
        return QueryClass::MultiHop;
    }
    QueryClass::Factoid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factoid_is_the_default() {
        assert_eq!(
            classify_query("error conexión postgres"),
            QueryClass::Factoid
        );
    }

    #[test]
    fn multi_hop_spanish() {
        assert_eq!(
            classify_query("cómo se relaciona MemoryIndustry con Falkor"),
            QueryClass::MultiHop
        );
        assert_eq!(
            classify_query("causas de la latencia"),
            QueryClass::MultiHop
        );
        assert_eq!(
            classify_query("de qué depende Trello"),
            QueryClass::MultiHop
        );
        assert_eq!(
            classify_query("relacion entre Backend FastAPI y el inventario"),
            QueryClass::MultiHop
        );
    }

    #[test]
    fn global_themes() {
        assert_eq!(
            classify_query("cuáles son los temas principales del corpus"),
            QueryClass::Global
        );
    }
}
