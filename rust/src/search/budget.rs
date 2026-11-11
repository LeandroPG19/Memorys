use std::sync::OnceLock;
use tiktoken_rs::{CoreBPE, cl100k_base};

fn encoder() -> &'static CoreBPE {
    static ENCODER: OnceLock<CoreBPE> = OnceLock::new();
    ENCODER.get_or_init(|| cl100k_base().expect("cl100k_base loads from embedded data"))
}

pub fn count_tokens(text: &str) -> usize {
    encoder().encode_with_special_tokens(text).len()
}

pub fn truncate_to_budget(text: &str, max_tokens: usize) -> String {
    let enc = encoder();
    let tokens = enc.encode_with_special_tokens(text);
    if tokens.len() <= max_tokens {
        return text.to_string();
    }
    let truncated = &tokens[..max_tokens];
    enc.decode(truncated).unwrap_or_else(|_| {
        let approx = max_tokens.saturating_mul(4);
        let mut end = approx.min(text.len());
        while !text.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        text[..end].to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_zero_for_empty() {
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn count_short_english() {
        let n = count_tokens("Hello world!");
        assert!(
            (1..=4).contains(&n),
            "Expected 1-4 for 'Hello world!', got {n}"
        );
    }

    #[test]
    fn count_handles_spanish_correctly() {
        let s = "El sistema MCP de cuba-memorys es excelente para agentes de IA.";
        let n = count_tokens(s);
        assert!(n > 0);
        assert!(n < 200, "sanity bound, got {n}");
    }

    #[test]
    fn truncate_preserves_short_text_unchanged() {
        let s = "short";
        assert_eq!(truncate_to_budget(s, 100), s);
    }

    #[test]
    fn truncate_returns_at_most_max_tokens() {
        let long = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.".repeat(20);
        let truncated = truncate_to_budget(&long, 50);
        assert!(count_tokens(&truncated) <= 50);
    }

    #[test]
    fn truncate_preserves_utf8_boundaries() {
        let s = "café".repeat(100);
        let t = truncate_to_budget(&s, 5);
        let _ = t.chars().count();
    }

    use std::fmt::Write as _;

    const WRITING: &str = "writing to a String cannot fail";

    /// CI-D4. The three tables are what `tiktoken-rs` 0.7.0 gave for the rows below, pasted from the block a
    /// test prints when its table is empty (`cargo test --workspace --lib search::budget -- --nocapture`).
    /// They are never edited by hand: a number that moves here is a budget that moves for every stored
    /// answer, and the library that moved it has to be the one that is refused, not the table.
    #[rustfmt::skip]
    const FIXED_IDS: &[(&str, &[u32])] = &[
        ("es_acentos", &[31282, 35, 1832, 43441, 60286, 1832, 1208, 34122, 3739, 30, 1717, 239, 438, 6792, 11, 31098, 2448, 3394, 379, 90164, 25, 36916, 220, 2366, 21, 13]),
        ("codigo", &[8998, 1925, 368, 314, 1095, 348, 25, 11564, 35937, 23, 29, 284, 7638, 21362, 15, 9448, 26, 220, 845, 5378, 14069, 90028, 85, 77575, 92, 5146, 335]),
        ("blancos_al_final", &[5589, 262]),
        ("blancos_en_serie", &[64, 1432, 2355, 65]),
        ("unicode", &[38623, 28584, 11, 356, 35170, 76502, 22656, 45918, 252, 11, 19216, 81, 8393, 24252, 11318, 30925, 22071, 5821, 11, 384, 54939]),
        ("endoftext", &[100257]),
        ("a_x150", &[70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 70540, 29558, 5418]),
        ("igual_x120", &[8315, 19311, 895]),
        ("mezcla_117", &[45202, 45202, 45202, 45202, 45202, 4513, 10961, 22, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 29289, 10266]),
    ];
    #[rustfmt::skip]
    const FIXED_COUNTS: &[(&str, usize)] = &[
        ("blancos_130", 3),
        ("espanol_4000", 3841),
        ("json_200", 2203),
    ];
    #[rustfmt::skip]
    const FIXED_PREFIXES: &[(&str, &str)] = &[
        ("cafe_x100", "cafécaféca"),
        ("espanol_4000", "El servidor guarda cada decisión con su motivo, y la próxima sesión la encuentra sin que nadie la repita. El servidor guarda cada decisión con su motivo, y la próxima sesión la encuentra sin que nadie la repita. El servidor"),
    ];

    fn spanish_long() -> String {
        "El servidor guarda cada decisión con su motivo, y la próxima sesión la encuentra sin que nadie la repita. "
            .repeat(160)
    }

    fn text_rows() -> Vec<(&'static str, String)> {
        vec![
            (
                "es_acentos",
                "¿Dónde quedó la decisión? Ñandú, pingüino y acción: año 2026.".to_string(),
            ),
            (
                "codigo",
                "fn main() { let v: Vec<u8> = vec![0xFF; 16]; println!(\"{v:?}\"); }".to_string(),
            ),
            ("blancos_al_final", "fin   ".to_string()),
            ("blancos_en_serie", "a\n\n\n  \nb".to_string()),
            (
                "unicode",
                "emoji 🙂, CJK 日本語, árabe مرحبا, e\u{301}".to_string(),
            ),
            ("endoftext", "<|endoftext|>".to_string()),
            ("a_x150", "a".repeat(150)),
            ("igual_x120", "=".repeat(120)),
            (
                "mezcla_117",
                format!("{}1234567{}", "x".repeat(40), "Y".repeat(70)),
            ),
        ]
    }

    fn long_rows() -> Vec<(&'static str, String)> {
        let objects: Vec<String> = (0..200)
            .map(|i| format!("{{\"id\":{i},\"nombre\":\"entidad {i}\"}}"))
            .collect();
        vec![
            ("blancos_130", format!("{}x", " ".repeat(130))),
            ("espanol_4000", spanish_long()),
            ("json_200", format!("[{}]", objects.join(","))),
        ]
    }

    fn cut_rows() -> Vec<(&'static str, String, usize)> {
        vec![
            ("cafe_x100", "café".repeat(100), 5),
            ("espanol_4000", spanish_long(), 50),
        ]
    }

    /// The ids of every short row, written as text so that ids, counts and prefixes are judged by one rule.
    /// The count the product reports has to agree with the ids the encoder gives, whatever the table says.
    fn id_rows() -> Vec<(&'static str, String)> {
        text_rows()
            .into_iter()
            .map(|(name, text)| {
                let ids = encoder().encode_with_special_tokens(&text);
                assert_eq!(
                    count_tokens(&text),
                    ids.len(),
                    "{name}: count_tokens and the encoder disagree on how many tokens it is"
                );
                (name, format!("{ids:?}"))
            })
            .collect()
    }

    fn count_rows() -> Vec<(&'static str, String)> {
        long_rows()
            .into_iter()
            .map(|(name, text)| (name, count_tokens(&text).to_string()))
            .collect()
    }

    fn pinned_ids() -> Vec<(&'static str, String)> {
        FIXED_IDS
            .iter()
            .map(|(name, ids)| (*name, format!("{ids:?}")))
            .collect()
    }

    fn pinned_counts() -> Vec<(&'static str, String)> {
        FIXED_COUNTS
            .iter()
            .map(|(name, count)| (*name, count.to_string()))
            .collect()
    }

    fn pinned_prefixes() -> Vec<(&'static str, String)> {
        FIXED_PREFIXES
            .iter()
            .map(|(name, cut)| (*name, format!("{cut:?}")))
            .collect()
    }

    /// The block to paste over the empty table `constant`, as `budget.rs` holds it. `reference` is what goes
    /// before each value: `&` for a slice, nothing for the rest.
    fn block(constant: &str, ty: &str, reference: &str, rows: &[(&str, String)]) -> String {
        let mut block = format!(
            "--- paste into rust/src/search/budget.rs, over `const {constant}` ---\n    \
             #[rustfmt::skip]\n    const {constant}: {ty} = &[\n"
        );
        for (name, value) in rows {
            writeln!(block, "        ({name:?}, {reference}{value}),").expect(WRITING);
        }
        write!(block, "    ];\n--- end of {constant} ---").expect(WRITING);
        block
    }

    /// An empty table is how the values are made: the blocks are printed and the test fails.
    fn refuse_empty_tables(blocks: &[String], a_table_is_empty: bool) {
        if a_table_is_empty {
            println!("\n{}", blocks.join("\n"));
            panic!(
                "a pinned table is empty: the blocks above are what the library gives today; paste \
                 them over the empty tables"
            );
        }
    }

    fn rows_that_differ(
        table: &str,
        got: &[(&str, String)],
        pinned: &[(&str, String)],
    ) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, value) in got {
            match pinned.iter().find(|row| row.0 == *name) {
                Some(row) if row.1 == *value => {}
                Some(row) => problems.push(format!(
                    "{table} {name}: got {value}, the pinned row is {}",
                    row.1
                )),
                None => problems.push(format!("{table} {name}: no pinned row")),
            }
        }
        problems
    }

    fn rows_with_no_input(
        table: &str,
        got: &[(&str, String)],
        pinned: &[(&str, String)],
    ) -> Vec<String> {
        pinned
            .iter()
            .filter(|row| !got.iter().any(|(name, _)| *name == row.0))
            .map(|row| format!("{table} {}: pinned, and no input makes it", row.0))
            .collect()
    }

    fn differences(table: &str, got: &[(&str, String)], pinned: &[(&str, String)]) -> Vec<String> {
        let mut problems = rows_that_differ(table, got, pinned);
        problems.extend(rows_with_no_input(table, got, pinned));
        problems
    }

    /// `cuts` are (case, what `truncate_to_budget` returned, the budget it was given).
    fn over_budget(cuts: &[(&str, String, usize)]) -> Vec<String> {
        cuts.iter()
            .filter_map(|(name, cut, budget)| {
                let kept = count_tokens(cut);
                (kept > *budget).then(|| {
                    format!("{name}: the prefix counts {kept} tokens, over its budget of {budget}")
                })
            })
            .collect()
    }

    /// The two examples of OpenAI's notebook «How to count tokens with tiktoken», coded with `cl100k_base`:
    /// a source that is not this repo, so a library that moves one of them is wrong and not the table.
    #[test]
    fn cl100k_gives_the_ids_openai_published_for_its_two_examples() {
        let enc = encoder();
        assert_eq!(
            enc.encode_with_special_tokens("Hello world!"),
            [9906, 1917, 0]
        );
        assert_eq!(
            enc.encode_with_special_tokens("tiktoken is great!"),
            [83, 1609, 5963, 374, 2294, 0]
        );
        assert_eq!(count_tokens("Hello world!"), 3);
        assert_eq!(count_tokens("tiktoken is great!"), 6);
    }

    #[test]
    fn the_fixed_table_counts_what_the_pinned_library_counted() {
        let (ids, counts) = (id_rows(), count_rows());
        let (want_ids, want_counts) = (pinned_ids(), pinned_counts());
        refuse_empty_tables(
            &[
                block("FIXED_IDS", "&[(&str, &[u32])]", "&", &ids),
                block("FIXED_COUNTS", "&[(&str, usize)]", "", &counts),
            ],
            want_ids.is_empty() || want_counts.is_empty(),
        );
        let mut problems = differences("FIXED_IDS", &ids, &want_ids);
        problems.extend(differences("FIXED_COUNTS", &counts, &want_counts));
        assert!(
            problems.is_empty(),
            "the fixed table no longer counts what the pinned library counted:\n{}",
            problems.join("\n")
        );
    }

    #[test]
    fn truncate_to_budget_returns_the_pinned_prefix_of_each_fixed_row() {
        let cuts: Vec<(&str, String, usize)> = cut_rows()
            .into_iter()
            .map(|(name, text, budget)| (name, truncate_to_budget(&text, budget), budget))
            .collect();
        let got: Vec<(&str, String)> = cuts
            .iter()
            .map(|(name, cut, _)| (*name, format!("{cut:?}")))
            .collect();
        let want = pinned_prefixes();
        refuse_empty_tables(
            &[block("FIXED_PREFIXES", "&[(&str, &str)]", "", &got)],
            want.is_empty(),
        );
        let mut problems = differences("FIXED_PREFIXES", &got, &want);
        problems.extend(over_budget(&cuts));
        let hello = truncate_to_budget("Hello world!", 2);
        if hello != "Hello world" {
            problems.push(format!(
                "\"Hello world!\" cut to 2 tokens gives {hello:?}, and OpenAI's ids [9906, 1917] are \"Hello world\""
            ));
        }
        assert!(
            problems.is_empty(),
            "truncate_to_budget no longer returns the pinned prefixes:\n{}",
            problems.join("\n")
        );
    }
}
