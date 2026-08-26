use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32String};

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

#[derive(Debug)]
struct SearchDocument {
    ticket_index: usize,
    text: Utf32String,
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
            documents: documents_for(tickets),
        }
    }

    pub fn replace_tickets(&mut self, tickets: &[Ticket]) {
        self.documents = documents_for(tickets);
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

fn documents_for(tickets: &[Ticket]) -> Arc<Vec<SearchDocument>> {
    Arc::new(
        tickets
            .iter()
            .enumerate()
            .map(|(ticket_index, ticket)| SearchDocument {
                ticket_index,
                text: ticket.searchable_text().into(),
            })
            .collect(),
    )
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

        let pattern = Pattern::new(
            &query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let mut matches: Vec<_> = documents
            .iter()
            .filter_map(|document| {
                pattern
                    .score(document.text.slice(..), &mut matcher)
                    .map(|score| SearchMatch {
                        ticket_index: document.ticket_index,
                        score,
                    })
            })
            .collect();
        matches.sort_by_key(|entry| std::cmp::Reverse(entry.score));

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
            created_at: "2026-01-01T00:00:00Z".into(),
            changed_at: "2026-01-02T00:00:00Z".into(),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
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
    fn searches_core_fields_and_ranks_best_title_match_first() {
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
    }

    #[test]
    fn latest_generation_can_replace_an_older_query() {
        let tickets = vec![ticket(1, "Alpha", ""), ticket(2, "Beta", "")];
        let mut engine = SearchEngine::new(&tickets);
        engine.submit("alpha");
        let generation = engine.submit("beta");

        let result = await_result(&engine, generation);

        assert_eq!(result.matches[0].ticket_index, 1);
    }
}
