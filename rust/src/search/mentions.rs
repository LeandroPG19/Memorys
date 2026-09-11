//! Mention detection for HippoRAG-style PPR seeds.
//! Match is lexical (normalized), not an LLM linker.

pub fn normalize_mention(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            '-' | '_' | '/' | '.' => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn name_mentioned_in_query(name: &str, query: &str) -> bool {
    let n = normalize_mention(name);
    if n.chars().count() < 4 {
        return false;
    }
    normalize_mention(query).contains(&n)
}

pub fn names_in_query<'a>(query: &str, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let q = normalize_mention(query);
    let mut hits: Vec<(usize, String)> = names
        .into_iter()
        .filter(|name| {
            let n = normalize_mention(name);
            n.chars().count() >= 4 && q.contains(&n)
        })
        .map(|name| (name.chars().count(), name.to_string()))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_, name) in hits {
        if seen.insert(name.to_ascii_lowercase()) {
            out.push(name);
        }
        if out.len() == 8 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphen_and_space_are_the_same() {
        assert!(name_mentioned_in_query(
            "Mapupita-Rust",
            "que producto se relaciona con Mapupita Rust"
        ));
    }

    #[test]
    fn short_names_do_not_match() {
        assert!(!name_mentioned_in_query("You", "who are you today"));
    }

    #[test]
    fn longer_name_wins_order() {
        let names = names_in_query(
            "hechos sobre Mapupita-Web y mapupita",
            ["mapupita", "Mapupita-Web", "Web"],
        );
        assert_eq!(names.first().map(String::as_str), Some("Mapupita-Web"));
        assert!(names.iter().any(|n| n == "mapupita"));
        assert!(!names.iter().any(|n| n == "Web"));
    }
}
