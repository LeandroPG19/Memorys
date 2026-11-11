// `cuba_docs` turns the HTML of a documentation page into the text it hands the agent, and `docs::grep`
// cuts that text at blank lines to find the paragraphs that name a word. Two things have to hold across a
// move of html2text: the paragraphs come out separated by a blank line with their words, their links and
// nothing of a `<script>` or a `<style>`, and a page that does not fit the width is text or an error of
// the tool, never a panic of the handler (html2text 0.12 `expect`s the conversion: a page nested deeper
// than the width gave `TooNarrow` and took the call down with it).
//
// The checks are properties of the text, never the text byte for byte: its decoration moves between
// versions (bold, `<noscript>`, lists) and that is not a defect.
//
// `html_to_text` returns a `String` up to html2text 0.12 and a `Result<String, _>` from 0.13. `Converted`
// reads either one, so the file compiles and judges the crate on both sides of that move without being
// edited: with the `String` it panics where the old crate does, with the `Result` it demands `Ok`.

use memory_industry::net::fetch::html_to_text;

trait Converted {
    fn converted(self) -> String;
}

impl Converted for String {
    fn converted(self) -> String {
        self
    }
}

impl<E: std::fmt::Debug> Converted for Result<String, E> {
    fn converted(self) -> String {
        self.expect("html_to_text has to convert the page, not fail on it")
    }
}

const WIDTH: usize = 100;

#[test]
fn a_documentation_page_keeps_its_paragraphs_its_content_and_its_links() {
    let html = "<html><head><style>.oculto { display: none }</style></head><body>\
                <h1>Guía</h1>\
                <p>Primer párrafo sobre tokio.</p>\
                <p>Segundo párrafo sobre axum.</p>\
                <p>Ver <a href=\"https://example.org/a\">la referencia</a>.</p>\
                <script>var secreto = 1;</script>\
                </body></html>";

    let text = html_to_text(html, WIDTH).converted();

    for wanted in [
        "Guía",
        "Primer párrafo sobre tokio.",
        "Segundo párrafo sobre axum.",
        "la referencia",
        "https://example.org/a",
    ] {
        assert!(
            text.contains(wanted),
            "the page's `{wanted}` is missing from its text:\n{text}"
        );
    }
    for unwanted in ["var secreto", ".oculto"] {
        assert!(
            !text.contains(unwanted),
            "`{unwanted}` belongs to a <script> or a <style> and is not page content:\n{text}"
        );
    }

    let blocks: Vec<&str> = text.split("\n\n").map(str::trim).collect();
    for paragraph in ["Primer párrafo sobre tokio.", "Segundo párrafo sobre axum."] {
        assert!(
            blocks.contains(&paragraph),
            "`{paragraph}` has to be a block of its own, between blank lines, because that is where \
             `docs::grep` cuts and keeps the neighbours of a hit. Blocks: {blocks:?}"
        );
    }

    for line in text.lines().filter(|line| line.contains("párrafo")) {
        assert!(
            line.chars().count() <= WIDTH,
            "a paragraph line is wider than the {WIDTH} columns it was asked for: {line:?}"
        );
    }
}

#[test]
fn a_page_nested_deeper_than_its_width_becomes_text_instead_of_taking_the_tool_down() {
    let depth = 80;
    let html = format!(
        "<html><body>{}fondo{}</body></html>",
        "<blockquote>".repeat(depth),
        "</blockquote>".repeat(depth)
    );

    let text = html_to_text(&html, WIDTH).converted();

    // Past the point where the quote prefixes eat the width, a word is hard-wrapped at the minimum
    // width and each piece carries the prefixes, so the word survives as its letters in order.
    let letters: String = text.chars().filter(|c| c.is_alphanumeric()).collect();
    assert!(
        letters.contains("fondo"),
        "the innermost text of a page nested {depth} levels deep has to survive, wrapped if it must: \
         {text:?}"
    );
}
