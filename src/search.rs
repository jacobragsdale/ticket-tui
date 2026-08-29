use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str, Utf32String};

use crate::model::Ticket;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub ticket_index: usize,
    pub score: u32,
}

#[derive(Debug)]
pub struct SearchResult {
    pub generation: u64,
    pub matches: Vec<SearchMatch>,
}

#[derive(Clone, Debug)]
struct SearchDocument {
    ticket_index: usize,
    text: Utf32String,
}

#[derive(Debug)]
pub struct SearchDocuments {
    documents: Arc<Vec<SearchDocument>>,
}

impl SearchDocuments {
    #[must_use]
    pub fn prepare(tickets: &[Ticket]) -> Self {
        Self {
            documents: Arc::new(
                tickets
                    .iter()
                    .enumerate()
                    .map(|(ticket_index, ticket)| SearchDocument {
                        ticket_index,
                        text: ticket.searchable_text().into(),
                    })
                    .collect(),
            ),
        }
    }
}

enum Command {
    Search {
        generation: u64,
        query: String,
        documents: Arc<Vec<SearchDocument>>,
    },
    Shutdown,
}

pub struct SearchEngine {
    sender: Sender<Command>,
    receiver: Receiver<SearchResult>,
    worker: Option<JoinHandle<()>>,
    generation: u64,
    documents: Arc<Vec<SearchDocument>>,
}

impl SearchEngine {
    #[must_use]
    pub fn new(tickets: &[Ticket]) -> Self {
        Self::from_documents(SearchDocuments::prepare(tickets))
    }

    #[must_use]
    pub fn from_documents(documents: SearchDocuments) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("ticket-search".into())
            .spawn(move || search_worker(command_receiver, result_sender))
            .expect("failed to start search worker");

        Self {
            sender: command_sender,
            receiver: result_receiver,
            worker: Some(worker),
            generation: 0,
            documents: documents.documents,
        }
    }

    pub fn replace_tickets(&mut self, tickets: &[Ticket]) {
        self.replace_documents(SearchDocuments::prepare(tickets));
    }

    pub fn replace_documents(&mut self, documents: SearchDocuments) {
        self.documents = documents.documents;
    }

    /// Re-indexes one work item an edit changed, leaving the rest of the
    /// documents as they are: a write-through edit touches one row, and
    /// rebuilding every document to follow it would be wasted work.
    pub fn update_document(&mut self, ticket_index: usize, ticket: &Ticket) {
        let documents = Arc::make_mut(&mut self.documents);
        if let Some(document) = documents
            .iter_mut()
            .find(|document| document.ticket_index == ticket_index)
        {
            document.text = ticket.searchable_text().into();
        }
    }

    pub fn submit(&mut self, query: &str) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let _ = self.sender.send(Command::Search {
            generation,
            query: query.to_owned(),
            documents: Arc::clone(&self.documents),
        });
        generation
    }

    pub fn try_result(&self) -> Option<SearchResult> {
        let mut latest = None;
        loop {
            match self.receiver.try_recv() {
                Ok(result) => latest = Some(result),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return latest,
            }
        }
    }
}

impl Drop for SearchEngine {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Highlights characters in a visible field that contribute to the current
/// fuzzy query. Each query atom is matched independently so a multi-word
/// search can light up different columns of the same row.
pub struct QueryHighlighter {
    pattern: Pattern,
    matcher: Matcher,
    buf: Vec<char>,
}

impl QueryHighlighter {
    #[must_use]
    pub fn new(query: &str) -> Self {
        Self {
            pattern: Pattern::new(
                query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            ),
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pattern.atoms.is_empty()
    }

    #[must_use]
    pub fn indices(&mut self, haystack: &str) -> Vec<u32> {
        if self.is_empty() || haystack.is_empty() {
            return Vec::new();
        }

        let mut indices = Vec::new();
        let haystack = Utf32Str::new(haystack, &mut self.buf);
        for atom in &self.pattern.atoms {
            let _ = atom.indices(haystack, &mut self.matcher, &mut indices);
        }
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

#[must_use]
pub fn match_char_indices(query: &str, haystack: &str) -> Vec<u32> {
    QueryHighlighter::new(query).indices(haystack)
}

fn search_worker(receiver: Receiver<Command>, sender: Sender<SearchResult>) {
    let mut matcher = Matcher::new(Config::DEFAULT);
    while let Ok(command) = receiver.recv() {
        let mut command = command;
        for newer in receiver.try_iter() {
            command = newer;
        }

        let Command::Search {
            generation,
            query,
            documents,
        } = command
        else {
            return;
        };

        let matches = score(&documents, &query, &mut matcher);

        if sender
            .send(SearchResult {
                generation,
                matches,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Scores prepared documents against one fuzzy query, best first.
fn score(documents: &[SearchDocument], query: &str, matcher: &mut Matcher) -> Vec<SearchMatch> {
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matches: Vec<_> = documents
        .iter()
        .filter_map(|document| {
            pattern
                .score(document.text.slice(..), matcher)
                .map(|score| SearchMatch {
                    ticket_index: document.ticket_index,
                    score,
                })
        })
        .collect();
    matches.sort_by_key(|entry| std::cmp::Reverse(entry.score));
    matches
}

/// Ranks work items against a fuzzy query on the calling thread, best first.
/// The engine above does the same work off the main thread so that typing
/// never waits for it; a one-shot read with no frames to draw has nothing to
/// wait for and asks here instead.
#[must_use]
pub fn rank(tickets: &[Ticket], query: &str) -> Vec<SearchMatch> {
    let documents = SearchDocuments::prepare(tickets);
    score(
        &documents.documents,
        query,
        &mut Matcher::new(Config::DEFAULT),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::model::TicketKey;

    fn ticket(id: i64, title: &str, description: &str) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id,
            },
            project: "atlas".into(),
            revision: 1,
            work_item_type: "Bug".into(),
            title: title.into(),
            state: "Active".into(),
            reason: None,
            assigned_to: Some("Avery Chen".into()),
            priority: Some(1),
            area_path: "Atlas\\Search".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: vec!["rust".into()],
            description: description.into(),
            description_html: String::new(),
            created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
            changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
            details_rev: 0,
        }
    }

    fn await_result(engine: &SearchEngine, generation: u64) -> SearchResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = engine.try_result()
                && result.generation == generation
            {
                return result;
            }
            assert!(Instant::now() < deadline, "search worker timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn search_ranks_the_best_title_match_first_and_the_latest_query_wins() {
        let tickets = vec![
            ticket(1, "Fix ticket search", "nothing special"),
            ticket(2, "Update deployment", "ticket search only appears here"),
            ticket(3, "Search indexing", "nothing special"),
        ];
        let mut engine = SearchEngine::new(&tickets);

        let generation = engine.submit("ticket search");
        let result = await_result(&engine, generation);

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].ticket_index, 0);

        engine.submit("update");
        let generation = engine.submit("indexing");
        let result = await_result(&engine, generation);
        assert_eq!(
            result.matches[0].ticket_index, 2,
            "the newest query replaces the one still in flight"
        );
    }

    fn matched_chars(query: &str, haystack: &str) -> String {
        let chars: Vec<char> = haystack.chars().collect();
        match_char_indices(query, haystack)
            .into_iter()
            .map(|index| chars[index as usize])
            .collect()
    }

    #[test]
    fn match_char_indices_highlights_one_atom_of_a_multi_word_query() {
        assert_eq!(
            matched_chars("search", "Fix ticket search").to_ascii_lowercase(),
            "search"
        );
        assert!(match_char_indices("bug", "Fix ticket search").is_empty());
        assert_eq!(
            matched_chars("bug search", "Fix ticket search").to_ascii_lowercase(),
            "search"
        );
        assert_eq!(
            matched_chars("bug search", "Bug").to_ascii_lowercase(),
            "bug"
        );
    }
}
