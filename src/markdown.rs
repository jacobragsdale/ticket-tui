//! Descriptions, out to an editor as Markdown and back as HTML.
//!
//! A description is the one long-form field a work item has, and the only one
//! written in a rich-text editor. Editing it in a one-line prompt is hopeless,
//! so the TUI hands it to `$EDITOR` instead — and an editor wants Markdown,
//! not the `<div>` soup Azure DevOps stores.
//!
//! Both directions are written here. [`html_to_markdown`] walks the stored
//! HTML with the same tokenizer the details pane reads it with and writes the
//! Markdown an author would have typed; [`markdown_to_html`] reads that back
//! and rebuilds the markup Azure DevOps takes. The pair round-trips the
//! formatting a description actually uses — paragraphs, bulleted and numbered
//! lists however deeply nested, links, inline code, fenced code blocks,
//! headings, bold, and rules.
//!
//! It does not round-trip everything. A table, an image, or a coloured span
//! has no Markdown here, and saving would replace it with the plain reading of
//! it, so a file holding any of those opens with [`RICH_FORMATTING_NOTICE`] at
//! the top and the author can quit without saving. The notice is taken off
//! again before anything is compared or converted, so leaving it in place
//! costs nothing.

use crate::html::{Tag, Visitor, decode_entities, walk};

/// The line that warns a description carries formatting this module cannot
/// write down. It is the first line of the file when it is there at all.
pub const RICH_FORMATTING_NOTICE: &str =
    "<!-- rich formatting in this description will be replaced on save -->";

/// The file the editor opens on: the notice when the description needs one,
/// then the description as Markdown.
#[must_use]
pub fn description_document(html: &str) -> String {
    let (markdown, rich) = render(html);
    if rich {
        format!("{RICH_FORMATTING_NOTICE}\n\n{markdown}")
    } else {
        markdown
    }
}

/// The Markdown one description reads as.
///
/// Paragraphs are separated by a blank line, `<ul>` items take `- ` and `<ol>`
/// items their number, nested lists indent two spaces a level, links read as
/// `[text](url)`, `<code>` keeps its backticks, `<pre>` becomes a fenced
/// block, headings take their `#`s, and `<b>` becomes `**bold**`.
#[must_use]
pub fn html_to_markdown(html: &str) -> String {
    render(html).0
}

/// Whether a description carries formatting [`html_to_markdown`] cannot write
/// down: a table, an image, or a tag carrying its own styling.
#[must_use]
pub fn has_rich_formatting(html: &str) -> bool {
    render(html).1
}

/// What an edited file says, with the notice line and whatever an editor did
/// to the end of it taken off, so a file that was opened and closed again
/// compares equal to the one that was written.
#[must_use]
pub fn saved_markdown(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    strip_notice(&text).trim_end().to_owned()
}

/// The document without the notice, and without the blank line under it. A
/// file whose first line is something else is its own body.
fn strip_notice(text: &str) -> &str {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    if first.trim() == RICH_FORMATTING_NOTICE {
        rest.strip_prefix('\n').unwrap_or(rest)
    } else {
        text
    }
}

fn render(html: &str) -> (String, bool) {
    let mut writer = MarkdownWriter::new(html.len());
    walk(html, &mut writer);
    writer.finish()
}

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

/// Tags with no Markdown of their own, whose content survives a save only as
/// the plain reading of it.
const RICH_TAGS: &[&str] = &[
    "table", "thead", "tbody", "tr", "td", "th", "img", "iframe", "video", "audio", "svg",
    "object", "embed",
];

/// Tags that carry nothing but styling, so one with attributes is a colour or
/// a font the Markdown cannot keep.
const STYLING_TAGS: &[&str] = &["span", "font", "mark"];

/// Writes Markdown as the walk hands it the document. The shape of it follows
/// `html::Renderer` closely — the two agree on where a line ends and where a
/// blank line goes — and differs only in what it writes at each boundary.
#[derive(Default)]
struct MarkdownWriter {
    out: String,
    /// The `<ul>` and `<ol>` elements open around the current position.
    lists: Vec<List>,
    /// One entry per open `<a>`: its target, and where its text started.
    links: Vec<(String, usize)>,
    /// The block boundary the markup has asked for but no content has needed
    /// yet, so a run of closing tags costs one break rather than four.
    pending: Break,
    /// Depth of `<pre>` nesting: inside one, whitespace is the author's.
    pre: usize,
    /// Whether a `<pre>` has just opened, so the line break editors write
    /// straight after the tag is the markup's rather than the author's.
    pre_start: bool,
    /// Cells written in the current table row, so every cell but the first is
    /// preceded by a separator.
    cells: usize,
    /// Whether anything seen so far has formatting Markdown cannot express.
    rich: bool,
}

impl MarkdownWriter {
    fn new(capacity: usize) -> Self {
        Self {
            out: String::with_capacity(capacity),
            ..Self::default()
        }
    }

    fn finish(self) -> (String, bool) {
        (self.out.trim().to_owned(), self.rich)
    }

    /// Notes formatting that will not survive a save, so the file can warn
    /// about it before anybody types into it.
    fn note_rich(&mut self, tag: &Tag<'_>) {
        if self.rich {
            return;
        }
        let name = tag.name.as_str();
        self.rich = RICH_TAGS.contains(&name)
            || (STYLING_TAGS.contains(&name) && !tag.attributes().trim().is_empty())
            || tag
                .attribute("style")
                .is_some_and(|style| !style.trim().is_empty());
    }

    fn tag(&mut self, tag: &Tag<'_>) {
        self.note_rich(tag);
        match tag.name.as_str() {
            "br" => self.hard_break(),
            "p" | "blockquote" | "table" => self.request(Break::Paragraph),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.heading(tag),
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
            "pre" => self.fence(tag),
            // Inside a `<pre>` the block is already fenced, so the backticks
            // would only be noise.
            "code" if self.pre == 0 => self.push("`"),
            "a" => self.link(tag),
            "b" | "strong" => self.push("**"),
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
                self.push("---");
                self.request(Break::Paragraph);
            }
            // Italics, spans, fonts, and everything unrecognised: the text is
            // the part worth keeping.
            _ => {}
        }
    }

    /// Opens or closes a heading. Markdown here goes three levels deep, which
    /// is as far as a description ever reasonably nests; anything below folds
    /// into `###`, and the plain reading of every heading is the same anyway.
    fn heading(&mut self, tag: &Tag<'_>) {
        self.request(Break::Paragraph);
        if tag.closing {
            return;
        }
        let level = tag.name[1..].parse::<usize>().unwrap_or(1).clamp(1, 3);
        self.push(&format!("{} ", "#".repeat(level)));
    }

    /// Opens or closes a `<pre>` as a fenced block, which is the one place
    /// where the author's own whitespace is kept.
    fn fence(&mut self, tag: &Tag<'_>) {
        if tag.closing {
            self.pre = self.pre.saturating_sub(1);
            if !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            self.out.push_str("```");
            self.request(Break::Paragraph);
        } else {
            self.request(Break::Paragraph);
            self.flush();
            self.out.push_str("```\n");
            self.pre += 1;
            self.pre_start = true;
        }
    }

    /// Opens or closes a list. A list at the top level stands apart from the
    /// prose around it; one nested inside an item only starts a new line, so
    /// the items stay a single block.
    fn list(&mut self, tag: &Tag<'_>) {
        let wanted = if self.lists.is_empty() {
            Break::Paragraph
        } else {
            Break::Line
        };
        if tag.closing {
            self.lists.pop();
            let wanted = if self.lists.is_empty() {
                Break::Paragraph
            } else {
                Break::Line
            };
            self.request(wanted);
        } else {
            self.request(wanted);
            self.lists.push(List {
                ordered: tag.name == "ol",
                item: 0,
            });
        }
    }

    /// Writes one list item's marker: `- ` for a bullet list, `1.`, `2.` for a
    /// numbered one, indented two spaces per level of nesting, which is how
    /// [`markdown_to_html`] reads the nesting back.
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
            _ => "- ".to_owned(),
        };
        self.push(&format!("{indent}{marker}"));
    }

    /// Opens an `<a>` on the hope of a `[text](url)`, which [`Self::close_link`]
    /// either finishes or takes back.
    fn link(&mut self, tag: &Tag<'_>) {
        if tag.closing {
            self.close_link();
            return;
        }
        self.flush();
        let href = tag.attribute("href").unwrap_or_default();
        self.out.push('[');
        let mark = self.out.len();
        self.links.push((href, mark));
    }

    /// Closes an `<a>`. A link with no target, or whose text already is its
    /// target, is written as the text alone: the brackets would say nothing.
    fn close_link(&mut self) {
        let Some((href, mark)) = self.links.pop() else {
            return;
        };
        let mark = mark.min(self.out.len());
        let text = self.out[mark..].trim().to_owned();
        if href.is_empty() {
            // The `[` was written in hope of a target that never came.
            self.out.remove(mark - 1);
            return;
        }
        if text.is_empty() || text == href {
            self.out.truncate(mark - 1);
            self.push(&href);
            return;
        }
        self.push(&format!("]({href})"));
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

    /// A `<br>`, which unlike a block boundary stacks: it is the blank line an
    /// editor writes between two lines of the same paragraph.
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

impl Visitor for MarkdownWriter {
    fn text(&mut self, raw: &str) {
        Self::text(self, raw);
    }

    fn tag(&mut self, tag: &Tag<'_>) {
        Self::tag(self, tag);
    }
}

/// Rebuilds the HTML Azure DevOps stores from the Markdown an editor saved.
///
/// A blank line ends a paragraph and the lines inside one are joined with
/// `<br>`; `- `, `* `, `1. `, and `1) ` are list items, nested two spaces a
/// level; ` ``` ` fences a `<pre>`; `#` through `###` are headings; `---` is a
/// rule; and `[text](url)`, `` `code` ``, and `**bold**` are what they look
/// like. Everything else is text, with `&`, `<`, and `>` escaped. An empty
/// document stays empty, which is how a description is cleared.
#[must_use]
pub fn markdown_to_html(markdown: &str) -> String {
    let normalized = markdown.replace("\r\n", "\n");
    let mut builder = HtmlBuilder::default();
    for line in normalized.lines() {
        builder.line(line);
    }
    builder.finish()
}

#[derive(Default)]
struct HtmlBuilder {
    out: String,
    /// The lines of the paragraph being read, joined with `<br>` when it ends.
    paragraph: Vec<String>,
    /// Whether each list open around the current item is ordered, outermost
    /// first. Every level holds one open `<li>`.
    lists: Vec<bool>,
    /// The lines of the fenced block being read, if a fence is open.
    fenced: Option<Vec<String>>,
}

impl HtmlBuilder {
    fn line(&mut self, line: &str) {
        if let Some(fenced) = self.fenced.as_mut() {
            if line.trim_start().starts_with("```") {
                self.close_fence();
            } else {
                fenced.push(line.to_owned());
            }
            return;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            self.close_blocks();
            self.fenced = Some(Vec::new());
        } else if trimmed.is_empty() {
            self.close_blocks();
        } else if let Some((level, text)) = heading(trimmed) {
            self.close_blocks();
            self.out
                .push_str(&format!("<h{level}>{}</h{level}>", inline(text, 0)));
        } else if is_rule(trimmed) {
            self.close_blocks();
            self.out.push_str("<hr>");
        } else if let Some((depth, ordered, text)) = list_item(line) {
            self.close_paragraph();
            self.item(depth, ordered, text);
        } else {
            self.close_lists();
            self.paragraph.push(trimmed.to_owned());
        }
    }

    fn finish(mut self) -> String {
        self.close_fence();
        self.close_blocks();
        self.out
    }

    /// Opens one list item at `depth`, closing and opening whatever lists that
    /// takes. A jump of more than one level deep is read as one level: two
    /// stray spaces are far likelier than an item with no parent.
    fn item(&mut self, depth: usize, ordered: bool, text: &str) {
        let depth = depth.min(self.lists.len());
        while self.lists.len() > depth + 1 {
            self.out.push_str("</li>");
            let ordered = self.lists.pop().unwrap_or(false);
            self.out.push_str(close_list(ordered));
        }
        if self.lists.len() == depth + 1 {
            self.out.push_str("</li>");
            if self.lists[depth] != ordered {
                self.lists.pop();
                self.out.push_str(close_list(!ordered));
                self.out.push_str(open_list(ordered));
                self.lists.push(ordered);
            }
        } else {
            self.out.push_str(open_list(ordered));
            self.lists.push(ordered);
        }
        self.out.push_str("<li>");
        self.out.push_str(&inline(text, 0));
    }

    fn close_blocks(&mut self) {
        self.close_paragraph();
        self.close_lists();
    }

    fn close_paragraph(&mut self) {
        if self.paragraph.is_empty() {
            return;
        }
        let lines: Vec<String> = std::mem::take(&mut self.paragraph)
            .iter()
            .map(|line| inline(line, 0))
            .collect();
        self.out.push_str("<p>");
        self.out.push_str(&lines.join("<br>"));
        self.out.push_str("</p>");
    }

    fn close_lists(&mut self) {
        while let Some(ordered) = self.lists.pop() {
            self.out.push_str("</li>");
            self.out.push_str(close_list(ordered));
        }
    }

    /// Closes the fenced block, if one is open. A fence left unclosed at the
    /// end of the file still holds code.
    fn close_fence(&mut self) {
        let Some(fenced) = self.fenced.take() else {
            return;
        };
        self.out.push_str("<pre>");
        self.out.push_str(&escape(&fenced.join("\n")));
        self.out.push_str("</pre>");
    }
}

const fn open_list(ordered: bool) -> &'static str {
    if ordered { "<ol>" } else { "<ul>" }
}

const fn close_list(ordered: bool) -> &'static str {
    if ordered { "</ol>" } else { "</ul>" }
}

/// The heading level and text of a `#` line, or `None` when the line is not
/// one. Levels below three read as three, which is as deep as the HTML goes.
fn heading(trimmed: &str) -> Option<(usize, &str)> {
    let hashes = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let text = trimmed[hashes..].strip_prefix(' ')?;
    Some((hashes.min(3), text.trim()))
}

/// Whether a line is a horizontal rule: three or more of `-` or `*`, nothing
/// else on it.
fn is_rule(trimmed: &str) -> bool {
    trimmed.len() >= 3
        && (trimmed.chars().all(|character| character == '-')
            || trimmed.chars().all(|character| character == '*'))
}

/// The nesting depth, kind, and text of a list item, or `None` when the line
/// is not one. Every two leading spaces is one level.
fn list_item(line: &str) -> Option<(usize, bool, &str)> {
    let rest = line.trim_start_matches(' ');
    let depth = (line.len() - rest.len()) / 2;
    if let Some(text) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
        return Some((depth, false, text.trim()));
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let after = &rest[digits..];
    let text = after
        .strip_prefix(". ")
        .or_else(|| after.strip_prefix(") "))?;
    Some((depth, true, text.trim()))
}

/// How far `[text](url)` and `**bold**` are followed into one another before
/// the rest is taken as plain text. Nothing an author writes nests this far;
/// a pathological file cannot make this recurse without end.
const MAX_INLINE_DEPTH: usize = 6;

/// Renders one line's inline markup: code spans, links, and bold, with
/// everything else escaped as the text it is.
fn inline(text: &str, depth: usize) -> String {
    let mut out = String::with_capacity(text.len());
    if depth >= MAX_INLINE_DEPTH {
        out.push_str(&escape(text));
        return out;
    }
    let mut rest = text;
    while let Some(index) = rest.find(['`', '[', '*']) {
        let (before, after) = rest.split_at(index);
        out.push_str(&escape(before));
        let taken = match after.as_bytes()[0] {
            b'`' => inline_code(after, &mut out),
            b'[' => inline_link(after, depth, &mut out),
            _ => inline_bold(after, depth, &mut out),
        };
        match taken {
            Some(length) => rest = &after[length..],
            None => {
                out.push_str(&escape(&after[..1]));
                rest = &after[1..];
            }
        }
    }
    out.push_str(&escape(rest));
    out
}

/// A `` `code` `` span, and how much of `rest` it took.
fn inline_code(rest: &str, out: &mut String) -> Option<usize> {
    let end = rest[1..].find('`')?;
    out.push_str(&format!("<code>{}</code>", escape(&rest[1..=end])));
    Some(end + 2)
}

/// A `[text](url)` link, and how much of `rest` it took.
fn inline_link(rest: &str, depth: usize, out: &mut String) -> Option<usize> {
    let close = rest.find("](")?;
    let end = rest[close..].find(')')? + close;
    let text = &rest[1..close];
    let href = &rest[close + 2..end];
    out.push_str(&format!(
        "<a href=\"{}\">{}</a>",
        escape_attribute(href),
        inline(text, depth + 1)
    ));
    Some(end + 1)
}

/// A `**bold**` run, and how much of `rest` it took.
fn inline_bold(rest: &str, depth: usize, out: &mut String) -> Option<usize> {
    let body = rest.strip_prefix("**")?;
    let end = body.find("**")?;
    out.push_str(&format!("<b>{}</b>", inline(&body[..end], depth + 1)));
    Some(end + 4)
}

/// The three characters that would otherwise be read as markup.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// The same, plus the quote that would end an attribute early.
fn escape_attribute(text: &str) -> String {
    escape(text).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::html_to_text;
    use pretty_assertions::assert_eq;

    /// One description of each shape the pair round-trips.
    fn fixtures() -> [&'static str; 3] {
        [
            concat!(
                "<p><b>Problem.</b> Descriptions are the one long-form field.</p>",
                "<p>Approach:</p>",
                "<ul>",
                "<li>Hand the description to <code>$EDITOR</code>.</li>",
                "<li>Read the Markdown back as HTML.</li>",
                "</ul>",
            ),
            concat!(
                "<h2>Steps</h2>",
                "<ol>",
                "<li>Open the <a href=\"https://dev.azure.com/demo\">board</a>.</li>",
                "<li>Pick a ticket:<ul><li>ready</li><li>blocked</li></ul></li>",
                "</ol>",
                "<p>Then run:</p>",
                "<pre>cargo test --all-targets</pre>",
            ),
            concat!(
                "<h1>Release</h1>",
                "<p>Ship it &amp; tell the team.</p>",
                "<hr>",
                "<p>Notes: <b>none</b>.</p>",
            ),
        ]
    }

    #[test]
    fn a_description_reads_as_paragraphs_bullets_code_and_bold() {
        assert_eq!(
            html_to_markdown(fixtures()[0]),
            concat!(
                "**Problem.** Descriptions are the one long-form field.\n",
                "\n",
                "Approach:\n",
                "\n",
                "- Hand the description to `$EDITOR`.\n",
                "- Read the Markdown back as HTML.",
            )
        );
    }

    #[test]
    fn headings_numbered_lists_nesting_links_and_fences_keep_their_shape() {
        assert_eq!(
            html_to_markdown(fixtures()[1]),
            concat!(
                "## Steps\n",
                "\n",
                "1. Open the [board](https://dev.azure.com/demo).\n",
                "2. Pick a ticket:\n",
                "  - ready\n",
                "  - blocked\n",
                "\n",
                "Then run:\n",
                "\n",
                "```\n",
                "cargo test --all-targets\n",
                "```",
            )
        );
        assert_eq!(
            html_to_markdown("<h1>One</h1><h5>Deep</h5><hr><div>a</div><div>b</div>"),
            "# One\n\n### Deep\n\n---\n\na\nb",
            "headings below three fold into ###, and div soup keeps its lines"
        );
        assert_eq!(
            html_to_markdown("<p>See <a href=\"https://x/y\"></a> and <a>nothing</a></p>"),
            "See https://x/y and nothing",
            "a link with no text of its own, and text with no link, lose the brackets"
        );
        assert_eq!(html_to_markdown(""), "");
    }

    #[test]
    fn markdown_builds_the_paragraphs_lists_links_code_and_headings_back() {
        assert_eq!(
            markdown_to_html("**Problem.** One line.\n\nApproach:"),
            "<p><b>Problem.</b> One line.</p><p>Approach:</p>"
        );
        assert_eq!(
            markdown_to_html("- one\n- two"),
            "<ul><li>one</li><li>two</li></ul>"
        );
        assert_eq!(
            markdown_to_html("1. one\n2. two:\n  - deep\n  - deeper"),
            "<ol><li>one</li><li>two:<ul><li>deep</li><li>deeper</li></ul></li></ol>",
            "two spaces nest a list inside the item above it"
        );
        assert_eq!(
            markdown_to_html("## Steps\n\n[board](https://dev.azure.com/demo?a=1&b=2)"),
            concat!(
                "<h2>Steps</h2>",
                "<p><a href=\"https://dev.azure.com/demo?a=1&amp;b=2\">board</a></p>",
            )
        );
        assert_eq!(
            markdown_to_html("Run `cargo test`:\n\n```\nif a < b {\n    ok();\n}\n```"),
            concat!(
                "<p>Run <code>cargo test</code>:</p>",
                "<pre>if a &lt; b {\n    ok();\n}</pre>",
            )
        );
        assert_eq!(
            markdown_to_html("one\ntwo"),
            "<p>one<br>two</p>",
            "a line break inside a paragraph is one"
        );
        assert_eq!(markdown_to_html("---"), "<hr>");
        assert_eq!(
            markdown_to_html("a & b < c"),
            "<p>a &amp; b &lt; c</p>",
            "the three characters that would be read as markup are escaped"
        );
        assert_eq!(markdown_to_html(""), "", "an empty file clears the field");
        assert_eq!(markdown_to_html("   \n\n  "), "");
    }

    #[test]
    fn a_description_that_goes_out_and_comes_back_reads_the_same() {
        for html in fixtures() {
            let markdown = html_to_markdown(html);
            let rebuilt = markdown_to_html(&markdown);
            assert_eq!(
                html_to_text(&rebuilt),
                html_to_text(html),
                "round trip of {html}\nvia\n{markdown}\ngave\n{rebuilt}"
            );
            assert!(
                !has_rich_formatting(html),
                "the fixtures are all in the supported subset"
            );
        }
    }

    #[test]
    fn only_formatting_that_cannot_be_written_down_earns_the_notice() {
        for plain in fixtures() {
            assert_eq!(description_document(plain), html_to_markdown(plain));
        }
        assert!(!has_rich_formatting(
            "<p>Plain <b>enough</b> <span>here</span></p>"
        ));

        for rich in [
            "<table><tr><td>a</td></tr></table>",
            "<p><img src=\"a.png\" alt=\"board\"></p>",
            "<p><span style=\"color:red\">warning</span></p>",
            "<p style=\"text-align:center\">centred</p>",
            "<p><font face=\"Comic Sans\">no</font></p>",
        ] {
            assert!(has_rich_formatting(rich), "{rich}");
            let document = description_document(rich);
            assert!(
                document.starts_with(RICH_FORMATTING_NOTICE),
                "the warning is the first line: {document}"
            );
            assert_eq!(
                saved_markdown(&document),
                html_to_markdown(rich),
                "the notice comes off again before anything is compared"
            );
        }
    }

    #[test]
    fn a_file_that_comes_back_untouched_reads_as_what_was_written() {
        let document = description_document("<table><tr><td>a</td></tr></table>");
        assert_eq!(
            saved_markdown(&format!("{document}\n")),
            saved_markdown(&document)
        );
        assert_eq!(
            saved_markdown(&document.replace('\n', "\r\n")),
            saved_markdown(&document),
            "an editor that writes CRLF has still changed nothing"
        );
        assert_eq!(
            saved_markdown("Just text\n"),
            "Just text",
            "a file with no notice is its own body"
        );
        assert_ne!(
            saved_markdown(&format!("{document}\nand more")),
            saved_markdown(&document)
        );
    }

    #[test]
    fn malformed_and_unusual_markup_never_panics() {
        let document = concat!(
            "<h1>Title &amp; more</h1><ol><li>one<ul><li><a href=\"https://x/y\">link</a>",
            "</li></ul></li></ol><pre>code &lt;here&gt;</pre><table><tr><td>a</td>",
            "<td>b</td></tr></table><p><img alt=\"pic\"><code>x</code>&#8212;done</p>",
        );
        for end in 0..=document.len() {
            if document.is_char_boundary(end) {
                let markdown = html_to_markdown(&document[..end]);
                let _ = markdown_to_html(&markdown);
            }
        }
        for text in [
            "[",
            "[]",
            "[](",
            "`",
            "**",
            "***a**",
            "```",
            "```\nunclosed",
            "- ",
            "1.",
            "#",
            "#no space",
            "  - deep with no parent",
            "&<>",
        ] {
            let html = markdown_to_html(text);
            let _ = html_to_markdown(&html);
        }
    }
}
