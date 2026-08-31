//! Azure DevOps rich text, laid out for a terminal.
//!
//! Descriptions and comments come back as whatever the browser editor wrote:
//! numbered and nested lists, links, headings, inline `<code>` and `<pre>`
//! blocks, tables, images, numeric and named entities, and a great deal of
//! `<div>` soup. Dropping every tag flattens all of that into a wall of text,
//! so this module walks the markup once with a small stack and renders the
//! structure it finds. The result is what search reads and the details pane
//! draws; the raw HTML is kept beside it in the database, so an edit can hand
//! Azure DevOps back the document it sent.
//!
//! Nothing here parses HTML strictly. Unknown tags are transparent — their
//! text survives, their markup does not — and malformed input (an unclosed
//! tag, a stray `<`) is rendered as the text it looks like rather than
//! dropped.

/// What a block boundary asks of the layout before the next content lands.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
enum Break {
    #[default]
    None,
    /// Start a new line: `<div>` soup, list items, table rows.
    Line,
    /// Leave one blank line: paragraphs, headings, tables, `<pre>` blocks.
    Paragraph,
}

/// One open `<ul>` or `<ol>`, and how many items it has numbered so far.
struct List {
    ordered: bool,
    item: usize,
}

/// Longest entity body worth looking at: `&middot;` and `&#x1F600;` both fit.
const MAX_ENTITY: usize = 12;

/// The named entities Azure DevOps's editor actually emits. Every replacement
/// is a single character, and an unknown name is left standing as it was
/// written rather than swallowed.
const NAMED_ENTITIES: &[(&str, char)] = &[
    ("nbsp", ' '),
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("mdash", '—'),
    ("ndash", '–'),
    ("hellip", '…'),
    ("copy", '©'),
    ("rsquo", '’'),
    ("lsquo", '‘'),
    ("rdquo", '”'),
    ("ldquo", '“'),
    ("bull", '•'),
    ("middot", '·'),
    ("times", '×'),
];

/// Renders Azure DevOps rich text as structured plain text.
///
/// Block elements become lines, paragraphs are separated by one blank line at
/// most, `<ul>` items keep their bullet and `<ol>` items are numbered, nested
/// lists indent two spaces per level, links render as `text (url)`, `<pre>`
/// keeps its whitespace, and the common entities are decoded.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let mut renderer = Renderer::new(html.len());
    walk(html, &mut renderer);
    renderer.finish()
}

/// What a walk of some markup hands each piece of it to. The plain text the
/// details pane draws and the Markdown a description is edited as are two
/// readings of the same documents, so the tokenizer below is written once and
/// each renderer only says what a text node and a tag mean to it.
pub(crate) trait Visitor {
    fn text(&mut self, raw: &str);
    fn tag(&mut self, tag: &Tag<'_>);
}

/// Walks `html` once, handing every text node and every tag to `visitor` in
/// the order they were written. Markup that does not parse as a tag is handed
/// over as the text it looks like, so nothing is dropped on the way.
pub(crate) fn walk(html: &str, visitor: &mut impl Visitor) {
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        visitor.text(&rest[..start]);
        let after = &rest[start + 1..];
        // A comment runs to `-->` however much markup it swallows on the way.
        if let Some(body) = after.strip_prefix("!--") {
            let Some(end) = body.find("-->") else {
                return;
            };
            rest = &body[end + 3..];
            continue;
        }
        let Some(end) = after.find('>') else {
            // A `<` with nothing closing it is text someone typed.
            visitor.text(&rest[start..]);
            return;
        };
        let raw = &after[..end];
        // A `<` inside what looked like a tag means the outer one was never a
        // tag at all: `a < b <br>` opens no element. Keep it as text and start
        // again from the inner `<`.
        if let Some(inner) = raw.find('<') {
            visitor.text(&rest[start..start + 1 + inner]);
            rest = &after[inner..];
            continue;
        }
        let literal = &rest[start..start + end + 2];
        rest = &after[end + 1..];
        // A doctype or processing instruction says nothing a reader wants.
        if raw.starts_with(['!', '?']) {
            continue;
        }
        match Tag::parse(raw) {
            Some(tag) => visitor.tag(&tag),
            None => visitor.text(literal),
        }
    }
    visitor.text(rest);
}

#[derive(Default)]
struct Renderer {
    out: String,
    /// The `<ul>` and `<ol>` elements open around the current position.
    lists: Vec<List>,
    /// One entry per open `<a>`: its target, and where its text started.
    links: Vec<(String, usize)>,
    /// The block boundary the markup has asked for but no content has needed
    /// yet, so a run of closing tags costs one break rather than four.
    pending: Break,
    /// Depth of `<pre>` nesting: inside one, whitespace is kept verbatim.
    pre: usize,
    /// Whether a `<pre>` has just opened, so the line break editors write
    /// straight after the tag is the markup's rather than the author's.
    pre_start: bool,
    /// Cells written in the current table row, so every cell but the first is
    /// preceded by a separator.
    cells: usize,
}

impl Renderer {
    fn new(capacity: usize) -> Self {
        Self {
            out: String::with_capacity(capacity),
            ..Self::default()
        }
    }

    fn finish(self) -> String {
        self.out.trim().to_owned()
    }

    fn tag(&mut self, tag: &Tag<'_>) {
        match tag.name.as_str() {
            "br" => self.hard_break(),
            "p" | "blockquote" | "table" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.request(Break::Paragraph);
            }
            "div" => self.request(Break::Line),
            "ul" | "ol" => self.list(tag),
            "li" => self.item(tag),
            "tr" => {
                self.cells = 0;
                self.request(Break::Line);
            }
            "td" | "th" if !tag.closing => {
                if self.cells > 0 {
                    self.trim_trailing_spaces();
                    self.push(" | ");
                }
                self.cells += 1;
            }
            "pre" => {
                if tag.closing {
                    self.pre = self.pre.saturating_sub(1);
                    self.request(Break::Paragraph);
                } else {
                    self.request(Break::Paragraph);
                    self.flush();
                    self.pre += 1;
                    self.pre_start = true;
                }
            }
            // Inside a `<pre>` the block is already verbatim, so the backticks
            // would only be noise.
            "code" if self.pre == 0 => self.push("`"),
            "a" => {
                if tag.closing {
                    self.close_link();
                } else {
                    self.flush();
                    self.links
                        .push((tag.attribute("href").unwrap_or_default(), self.out.len()));
                }
            }
            "img" if !tag.closing => {
                let alt = tag.attribute("alt").unwrap_or_default();
                let alt = alt.trim();
                if alt.is_empty() {
                    self.push("[image]");
                } else {
                    self.push(&format!("[image: {alt}]"));
                }
            }
            "hr" if !tag.closing => {
                self.request(Break::Paragraph);
                self.push("───");
                self.request(Break::Paragraph);
            }
            // Bold, italics, spans, fonts, and everything unrecognised: the
            // text is the part worth keeping.
            _ => {}
        }
    }

    /// Opens or closes a list. A list at the top level stands apart from the
    /// prose around it; one nested inside an item only starts a new line, so
    /// the items stay a single block.
    fn list(&mut self, tag: &Tag<'_>) {
        if tag.closing {
            self.lists.pop();
            let wanted = if self.lists.is_empty() {
                Break::Paragraph
            } else {
                Break::Line
            };
            self.request(wanted);
        } else {
            let wanted = if self.lists.is_empty() {
                Break::Paragraph
            } else {
                Break::Line
            };
            self.request(wanted);
            self.lists.push(List {
                ordered: tag.name == "ol",
                item: 0,
            });
        }
    }

    /// Writes one list item's marker: `•` for a bullet list, `1.`, `2.` for a
    /// numbered one, indented two spaces per level of nesting. An item outside
    /// any list — the markup is not always well formed — is a bullet.
    fn item(&mut self, tag: &Tag<'_>) {
        self.request(Break::Line);
        if tag.closing {
            return;
        }
        let indent = "  ".repeat(self.lists.len().saturating_sub(1));
        let marker = match self.lists.last_mut() {
            Some(list) if list.ordered => {
                list.item += 1;
                format!("{}. ", list.item)
            }
            _ => "• ".to_owned(),
        };
        self.push(&format!("{indent}{marker}"));
    }

    /// Closes an `<a>`, appending its target unless the text already is the
    /// target. A link with no text of its own renders as the bare URL.
    fn close_link(&mut self) {
        let Some((href, mark)) = self.links.pop() else {
            return;
        };
        if href.is_empty() {
            return;
        }
        let text = self.out[mark.min(self.out.len())..].trim().to_owned();
        if text.is_empty() {
            self.push(&href);
        } else if text != href {
            self.push(&format!(" ({href})"));
        }
    }

    /// Writes one text node. Outside a `<pre>` a run of whitespace is one
    /// space, the way a browser would lay it out; inside one every space and
    /// line break is the author's.
    fn text(&mut self, raw: &str) {
        if raw.is_empty() {
            return;
        }
        let decoded = decode_entities(raw);
        if self.pre > 0 {
            let mut content = decoded.as_str();
            if std::mem::take(&mut self.pre_start) {
                content = content
                    .strip_prefix("\r\n")
                    .or_else(|| content.strip_prefix('\n'))
                    .unwrap_or(content);
            }
            if content.is_empty() {
                return;
            }
            self.flush();
            self.out.push_str(content);
            return;
        }
        let spaced_before = decoded.starts_with(char::is_whitespace);
        let spaced_after = decoded.ends_with(char::is_whitespace);
        let mut words = decoded.split_whitespace();
        let Some(first) = words.next() else {
            // Whitespace alone still separates two inline tags, unless a block
            // boundary is about to swallow it.
            if self.pending == Break::None && self.mid_line() {
                self.out.push(' ');
            }
            return;
        };
        self.flush();
        if spaced_before && self.mid_line() {
            self.out.push(' ');
        }
        self.out.push_str(first);
        for word in words {
            self.out.push(' ');
            self.out.push_str(word);
        }
        if spaced_after {
            self.out.push(' ');
        }
    }

    /// Whether the next character would land beside text already written.
    fn mid_line(&self) -> bool {
        !self.out.is_empty() && !self.out.ends_with([' ', '\n'])
    }

    fn push(&mut self, text: &str) {
        self.flush();
        self.out.push_str(text);
    }

    fn request(&mut self, wanted: Break) {
        self.pending = self.pending.max(wanted);
    }

    /// Applies the boundary the markup asked for. Nothing is written at the
    /// start of the document, and repeated boundaries never stack past one
    /// blank line.
    fn flush(&mut self) {
        let newlines = match std::mem::take(&mut self.pending) {
            Break::None => return,
            Break::Line => 1,
            Break::Paragraph => 2,
        };
        self.trim_trailing_spaces();
        if self.out.is_empty() {
            return;
        }
        for _ in self.trailing_newlines()..newlines {
            self.out.push('\n');
        }
    }

    /// A `<br>`, which unlike a block boundary stacks: `<div><br></div>`
    /// between two lines is how the editor writes a blank one.
    fn hard_break(&mut self) {
        self.flush();
        self.trim_trailing_spaces();
        if !self.out.is_empty() && self.trailing_newlines() < 2 {
            self.out.push('\n');
        }
    }

    /// Drops the spaces a line ended on: they came from the gaps between tags
    /// rather than from anything the author typed.
    fn trim_trailing_spaces(&mut self) {
        if self.pre > 0 {
            return;
        }
        while self.out.ends_with([' ', '\t']) {
            self.out.pop();
        }
    }

    fn trailing_newlines(&self) -> usize {
        self.out
            .chars()
            .rev()
            .take_while(|character| *character == '\n')
            .count()
    }
}

impl Visitor for Renderer {
    fn text(&mut self, raw: &str) {
        Self::text(self, raw);
    }

    fn tag(&mut self, tag: &Tag<'_>) {
        Self::tag(self, tag);
    }
}

/// One tag, reduced to the parts a renderer reads.
pub(crate) struct Tag<'a> {
    pub(crate) name: String,
    pub(crate) closing: bool,
    attributes: &'a str,
}

impl<'a> Tag<'a> {
    /// Parses the text between `<` and `>`, or returns `None` when it does not
    /// begin like a tag name — `a < 5` is arithmetic, not markup.
    fn parse(raw: &'a str) -> Option<Self> {
        let trimmed = raw.trim_start();
        let (closing, body) = match trimmed.strip_prefix('/') {
            Some(rest) => (true, rest.trim_start()),
            None => (false, trimmed),
        };
        if !body.starts_with(|character: char| character.is_ascii_alphabetic()) {
            return None;
        }
        let split = body
            .find(|character: char| !character.is_ascii_alphanumeric())
            .unwrap_or(body.len());
        let (name, attributes) = body.split_at(split);
        Some(Self {
            name: name.to_ascii_lowercase(),
            closing,
            attributes,
        })
    }

    /// Everything written between the tag name and the `>`, as it stands.
    /// Enough to tell a bare `<span>` from one carrying a colour.
    pub(crate) fn attributes(&self) -> &str {
        self.attributes
    }

    /// The value of one attribute, with its entities decoded. Quoted and bare
    /// values both parse, and a name that only appears inside another value is
    /// not mistaken for the attribute itself.
    pub(crate) fn attribute(&self, wanted: &str) -> Option<String> {
        let lowered = self.attributes.to_ascii_lowercase();
        let mut from = 0;
        while let Some(offset) = lowered[from..].find(wanted) {
            let start = from + offset;
            from = start + wanted.len();
            let standalone = start == 0
                || lowered[..start].ends_with(|character: char| character.is_whitespace());
            if !standalone {
                continue;
            }
            if let Some(value) = self.attributes[from..].trim_start().strip_prefix('=') {
                return Some(attribute_value(value.trim_start()));
            }
        }
        None
    }
}

fn attribute_value(raw: &str) -> String {
    let value = if let Some(rest) = raw.strip_prefix('"') {
        rest.split('"').next().unwrap_or_default()
    } else if let Some(rest) = raw.strip_prefix('\'') {
        rest.split('\'').next().unwrap_or_default()
    } else {
        raw.split(|character: char| character.is_whitespace())
            .next()
            .unwrap_or_default()
    };
    decode_entities(value)
}

/// Decodes the entities in one text node: `&#8217;` and `&#x2014;` by their
/// code point, the names in [`NAMED_ENTITIES`] by table. Anything else — a
/// bare `&`, an unknown name — is left exactly as it was written, and each
/// entity is decoded once, so `&amp;lt;` is the text `&lt;`.
pub(crate) fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let decoded = after
            .find(';')
            .filter(|end| *end <= MAX_ENTITY)
            .map(|end| &after[..end])
            .and_then(|body| decode_entity(body).map(|character| (body.len(), character)));
        match decoded {
            Some((length, character)) => {
                out.push(character);
                rest = &after[length + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(body: &str) -> Option<char> {
    if let Some(number) = body.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    let lowered = body.to_ascii_lowercase();
    NAMED_ENTITIES
        .iter()
        .find(|(name, _)| *name == lowered)
        .map(|(_, character)| *character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_roadmap_ticket_reads_as_paragraphs_bullets_and_inline_code() {
        let html = concat!(
            "<p><b>Problem.</b> Every description flattens into a wall of text.</p>",
            "<p>Approach:</p>",
            "<ul>",
            "<li>Walk the markup once in <code>html_to_text</code>.</li>",
            "<li>Keep the raw HTML so the editor can round&#8209;trip it.</li>",
            "</ul>",
            "<p>Done when a ticket&rsquo;s own description reads well &mdash; ",
            "paragraphs, bullets &amp; code.</p>",
        );

        assert_eq!(
            html_to_text(html),
            concat!(
                "Problem. Every description flattens into a wall of text.\n",
                "\n",
                "Approach:\n",
                "\n",
                "• Walk the markup once in `html_to_text`.\n",
                "• Keep the raw HTML so the editor can round\u{2011}trip it.\n",
                "\n",
                "Done when a ticket\u{2019}s own description reads well \u{2014} ",
                "paragraphs, bullets & code.",
            )
        );
    }

    #[test]
    fn div_soup_keeps_its_lines_and_a_bare_break_keeps_its_blank_one() {
        assert_eq!(
            html_to_text("<div>Line one</div><div><br></div><div>Line two<br>Line three</div>"),
            "Line one\n\nLine two\nLine three"
        );
        assert_eq!(
            html_to_text("<div>Looks&nbsp;good</div>\n<div>Shipping it</div>"),
            "Looks good\nShipping it",
            "the whitespace between two blocks is not a line of its own"
        );
    }

    #[test]
    fn headings_lists_links_code_blocks_tables_and_images_keep_their_shape() {
        let html = concat!(
            "<h2>Steps</h2>",
            "<ol>",
            "<li>Open the <a href=\"https://dev.azure.com/demo\">board</a>.</li>",
            "<li>Pick a ticket:<ul><li>ready</li><li>blocked</li></ul></li>",
            "</ol>",
            "<pre>fn main() {\n    println!(\"hi\");\n}</pre>",
            "<table>",
            "<tr><th>Field</th><th>Value</th></tr>",
            "<tr><td>State</td><td>Active</td></tr>",
            "</table>",
            "<p><img src=\"a.png\" alt=\"the board\"> and <img src=\"b.png\"></p>",
            "<hr>",
        );

        assert_eq!(
            html_to_text(html),
            concat!(
                "Steps\n",
                "\n",
                "1. Open the board (https://dev.azure.com/demo).\n",
                "2. Pick a ticket:\n",
                "  • ready\n",
                "  • blocked\n",
                "\n",
                "fn main() {\n",
                "    println!(\"hi\");\n",
                "}\n",
                "\n",
                "Field | Value\n",
                "State | Active\n",
                "\n",
                "[image: the board] and [image]\n",
                "\n",
                "───",
            )
        );
        assert_eq!(
            html_to_text(
                "<p>Before</p><pre>\nline one\n  line two\n</pre><blockquote>After</blockquote>"
            ),
            "Before\n\nline one\n  line two\n\nAfter",
            "the line break an editor writes straight after <pre> is markup, not content"
        );
    }

    #[test]
    fn a_link_whose_text_is_its_target_is_not_written_twice() {
        assert_eq!(
            html_to_text(
                "<a href=\"https://example.com/a?x=1&amp;y=2\">https://example.com/a?x=1&amp;y=2</a>"
            ),
            "https://example.com/a?x=1&y=2"
        );
        assert_eq!(
            html_to_text("<p>See <a href=\"https://example.com\"></a></p>"),
            "See https://example.com",
            "a link with no text of its own still has a target worth reading"
        );
    }

    #[test]
    fn malformed_markup_keeps_its_text_and_never_panics() {
        assert_eq!(
            html_to_text("<p>Unclosed <b>bold and a stray < five &unknown; <ul><li>item"),
            "Unclosed bold and a stray < five &unknown;\n\n• item"
        );
        assert_eq!(html_to_text("trailing <"), "trailing <");
        assert_eq!(html_to_text("<p>open <em>and gone"), "open and gone");
        assert_eq!(html_to_text("</li></ul></p>"), "");
        assert_eq!(html_to_text("<li>orphan</li>"), "• orphan");
        assert_eq!(html_to_text("<!-- hidden --><p>shown</p>"), "shown");
        assert_eq!(html_to_text("<!-- never closed <p>gone"), "");
        assert_eq!(html_to_text(""), "");
        assert_eq!(html_to_text("   \n  "), "");

        // A description arrives whole or not at all, but the renderer walks
        // whatever bytes it is handed, so every truncation has to be safe.
        let document = concat!(
            "<h1>Title &amp; more</h1><ol><li>one<ul><li><a href=\"https://x/y\">link</a>",
            "</li></ul></li></ol><pre>code &lt;here&gt;</pre><table><tr><td>a</td>",
            "<td>b</td></tr></table><p><img alt=\"pic\"><code>x</code>&#8212;done</p>",
        );
        for end in 0..=document.len() {
            if document.is_char_boundary(end) {
                let _ = html_to_text(&document[..end]);
            }
        }
    }

    #[test]
    fn entities_decode_once_by_name_and_by_number() {
        assert_eq!(
            html_to_text("&amp;lt; &#8217; &#x2014; &hellip; &BULL; &nosuch; &#zz; &"),
            "&lt; \u{2019} \u{2014} \u{2026} • &nosuch; &#zz; &"
        );
        assert_eq!(
            html_to_text("<pre>if (a &lt; b) { &amp;c }</pre>"),
            "if (a < b) { &c }",
            "a code block decodes its entities but keeps its spacing"
        );
    }

    #[test]
    fn blank_lines_never_stack_and_unknown_tags_are_transparent() {
        assert_eq!(
            html_to_text("<p>a</p><br><br><br><div></div><p>b</p>"),
            "a\n\nb"
        );
        assert_eq!(
            html_to_text(
                "<section><span style=\"color:red\">red</span> <font>text</font></section>"
            ),
            "red text"
        );
    }
}
