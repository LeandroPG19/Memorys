use std::collections::{HashMap, HashSet};

pub const RRF_K: f64 = 60.0;

pub fn query_entropy(query: &str) -> f64 {
    let words: Vec<&str> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let total = words.len();
    if total == 0 {
        return 0.0;
    }
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for w in &words {
        *freq.entry(w).or_default() += 1;
    }
    let mut entropy = 0.0;
    for &count in freq.values() {
        let p = count as f64 / total as f64;
        entropy -= p * p.log2();
    }
    entropy
}

pub fn content_overlap(a: &str, b: &str) -> f64 {
    let tokenize = |s: &str| -> HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(String::from)
            .collect()
    };
    let words_a = tokenize(a);
    let words_b = tokenize(b);
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }
    let intersection = words_a.intersection(&words_b).count();
    intersection as f64 / words_a.len().min(words_b.len()) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_entropy_uniform() {
        let e = query_entropy("rust is fast and safe for systems programming");
        assert!(e > 2.5, "diverse query should have high entropy: got {e}");
    }

    #[test]
    fn test_query_entropy_repetitive() {
        let e = query_entropy("hello hello hello");
        assert!(
            e < 0.01,
            "repetitive query should have near-zero entropy: got {e}"
        );
    }

    #[test]
    fn test_query_entropy_punctuation_invariant() {
        let e_clean = query_entropy("rust is fast");
        let e_punct = query_entropy("rust! is fast.");
        assert!(
            (e_clean - e_punct).abs() < 1e-9,
            "punctuation should not affect entropy: clean={e_clean}, punct={e_punct}"
        );
    }

    #[test]
    fn test_query_entropy_multilingual_punctuation() {
        let e1 = query_entropy("configuracion sistema");
        let e2 = query_entropy("configuracion. sistema,");
        assert!(
            (e1 - e2).abs() < 1e-9,
            "trailing punctuation should not change entropy: {e1} vs {e2}"
        );
    }

    #[test]
    fn test_text_overlap_identical() {
        assert!((content_overlap("hello world", "hello world") - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_text_overlap_disjoint() {
        assert_eq!(content_overlap("hello world", "foo bar"), 0.0);
    }

    #[test]
    fn test_rrf_k_is_pub_and_canonical() {
        assert_eq!(RRF_K, 60.0, "canonical k=60 (Cormack 2009)");
        let score_rank0 = 1.0 / (RRF_K + 1.0);
        assert!(
            (score_rank0 - 1.0 / 61.0).abs() < 1e-15,
            "rank-0 score must equal 1/61: {score_rank0}"
        );
    }

    #[test]
    fn empty_or_punctuation_only_query_has_zero_entropy() {
        assert_eq!(query_entropy(""), 0.0);
        assert_eq!(query_entropy("!!! ???"), 0.0);
    }

    #[test]
    fn shannon_uses_p_times_log_p_not_p_plus_log_p() {
        // Tokens a,a,a,b → p=(3/4,1/4). H ≈ 0.811.
        // Replacing `p * p.log2()` with `p + p.log2()` yields ≈ 1.811.
        let e = query_entropy("a a a b");
        let p1 = 0.75_f64;
        let p2 = 0.25_f64;
        let expected = -(p1 * p1.log2() + p2 * p2.log2());
        assert!(
            (e - expected).abs() < 1e-9,
            "Shannon H(3/4,1/4) expected {expected}, got {e}"
        );
    }

    #[test]
    fn empty_side_has_zero_overlap() {
        assert_eq!(content_overlap("", "hello world"), 0.0);
        assert_eq!(content_overlap("hello world", ""), 0.0);
        assert_eq!(content_overlap("!!!", "hello"), 0.0);
    }

    #[test]
    fn overlap_is_case_insensitive_and_partial() {
        let o = content_overlap("Hello World", "hello there");
        assert!((o - 0.5).abs() < 1e-9, "one shared token of two: {o}");
    }
}
