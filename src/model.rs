use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::timestamp::Timestamp;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TicketKey {
    pub organization: String,
    pub id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ticket {
    pub key: TicketKey,
    pub project: String,
    pub revision: i64,
    pub work_item_type: String,
    pub title: String,
    pub state: String,
    pub reason: Option<String>,
    pub assigned_to: Option<String>,
    pub priority: Option<i64>,
    pub area_path: String,
    pub iteration_path: String,
    pub tags: Vec<String>,
    pub description: String,
    pub created_at: Timestamp,
    pub changed_at: Timestamp,
    pub web_url: String,
    /// The revision whose comments and history are stored for this work item,
    /// or `0` when none have ever been read. Anything below `revision` means
    /// the details on file are behind the work item and are fetched again.
    pub details_rev: i64,
}

impl Ticket {
    #[must_use]
    pub fn searchable_text(&self) -> String {
        format!(
            "{} {} {} {} {} {} {} {}",
            self.key.id,
            self.title,
            self.assigned_to.as_deref().unwrap_or_default(),
            self.state,
            self.work_item_type,
            self.area_path,
            self.iteration_path,
            self.tags.join(" ")
        )
    }
}

/// The last segment of an area or iteration path: `demo\Sprint 1` -> `Sprint 1`.
///
/// Azure DevOps writes these paths with backslashes, but hand-written filters
/// and imported data use forward slashes, so both separate segments.
#[must_use]
pub fn path_leaf(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Where a work item state sits in the Azure DevOps state-category model.
///
/// Every process template (Agile, Basic, Scrum, CMMI) names its states
/// differently, so the raw string is mapped onto a shared category before it is
/// used for colouring or done/not-done decisions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StateCategory {
    Proposed,
    InProgress,
    Resolved,
    Completed,
    Removed,
    Unknown,
}

impl StateCategory {
    /// Classify a work item state name, ignoring case and surrounding space.
    #[must_use]
    pub fn of(state: &str) -> Self {
        match state.trim().to_ascii_lowercase().as_str() {
            "new" | "to do" | "proposed" | "approved" | "open" => Self::Proposed,
            "active" | "doing" | "in progress" | "committed" | "in review" => Self::InProgress,
            "resolved" | "ready for test" => Self::Resolved,
            "done" | "closed" | "completed" => Self::Completed,
            "removed" | "cut" | "rejected" => Self::Removed,
            _ => Self::Unknown,
        }
    }

    /// The category names Azure DevOps itself uses, in
    /// `/_apis/wit/workitemtypes/{type}/states` and in the database.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "proposed" => Self::Proposed,
            "inprogress" | "in progress" => Self::InProgress,
            "resolved" => Self::Resolved,
            "completed" => Self::Completed,
            "removed" => Self::Removed,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "Proposed",
            Self::InProgress => "InProgress",
            Self::Resolved => "Resolved",
            Self::Completed => "Completed",
            Self::Removed => "Removed",
            Self::Unknown => "Unknown",
        }
    }

    /// Where the category sits in the order a workflow runs, which is the
    /// order the state picker falls back to when nothing is cached.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Proposed => 0,
            Self::InProgress => 1,
            Self::Resolved => 2,
            Self::Completed => 3,
            Self::Removed => 4,
            Self::Unknown => 5,
        }
    }
}

/// One state a work item type can be moved to, and the category that colours
/// it. The order the picker shows is the order Azure DevOps listed them in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateOption {
    pub name: String,
    pub category: StateCategory,
}

impl StateOption {
    /// A state named without a category from Azure DevOps, classified the same
    /// way the table classifies the state it already holds.
    #[must_use]
    pub fn of(name: impl Into<String>) -> Self {
        let name = name.into();
        let category = StateCategory::of(&name);
        Self { name, category }
    }

    #[must_use]
    pub fn new(name: impl Into<String>, category: StateCategory) -> Self {
        Self {
            name: name.into(),
            category,
        }
    }
}

/// One person Azure DevOps can assign work to: the name a work item shows, and
/// the sign-in address behind it. Both resolve on a write, so the unique name
/// is used when it is known and the display name stands in when it is not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub display_name: String,
    /// `None` for somebody only ever seen in an `assigned_to` cell, which
    /// carries no address.
    pub unique_name: Option<String>,
}

impl Identity {
    #[must_use]
    pub fn new(display_name: impl Into<String>, unique_name: Option<String>) -> Self {
        Self {
            display_name: display_name.into(),
            unique_name,
        }
    }
}

/// The states each work item type allows, as Azure DevOps lists them. Empty
/// until a sync has fetched them, which is why the picker also has a fallback.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StateCatalog {
    by_type: HashMap<String, Vec<StateOption>>,
}

impl StateCatalog {
    pub fn insert(&mut self, work_item_type: impl Into<String>, states: Vec<StateOption>) {
        self.by_type.insert(work_item_type.into(), states);
    }

    /// The cached states for one work item type, or nothing when none were
    /// stored for it.
    #[must_use]
    pub fn states_for(&self, work_item_type: &str) -> &[StateOption] {
        self.by_type
            .get(work_item_type)
            .map_or(&[][..], Vec::as_slice)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_type.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    #[default]
    Changed,
    Priority,
    Id,
    Title,
    State,
    Type,
    Assignee,
    Organization,
    Project,
    Area,
    Iteration,
    Created,
    Tags,
}

impl SortField {
    pub const ALL: [Self; 13] = [
        Self::Changed,
        Self::Priority,
        Self::Id,
        Self::Title,
        Self::State,
        Self::Type,
        Self::Assignee,
        Self::Organization,
        Self::Project,
        Self::Area,
        Self::Iteration,
        Self::Created,
        Self::Tags,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Changed => "Changed",
            Self::Priority => "Priority",
            Self::Id => "ID",
            Self::Title => "Title",
            Self::State => "State",
            Self::Type => "Type",
            Self::Assignee => "Assignee",
            Self::Organization => "Org",
            Self::Project => "Project",
            Self::Area => "Area",
            Self::Iteration => "Iteration",
            Self::Created => "Created",
            Self::Tags => "Tags",
        }
    }

    #[must_use]
    pub const fn is_numeric(self) -> bool {
        matches!(self, Self::Priority)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RelationKind {
    Parent,
    Child,
    Related,
    Predecessor,
    Successor,
    Duplicate,
}

impl RelationKind {
    pub const ALL: [Self; 6] = [
        Self::Parent,
        Self::Child,
        Self::Related,
        Self::Predecessor,
        Self::Successor,
        Self::Duplicate,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Child => "child",
            Self::Related => "related",
            Self::Predecessor => "predecessor",
            Self::Successor => "successor",
            Self::Duplicate => "duplicate",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Parent => "Parent",
            Self::Child => "Child",
            Self::Related => "Related",
            Self::Predecessor => "Predecessor",
            Self::Successor => "Successor",
            Self::Duplicate => "Duplicate",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "parent" => Self::Parent,
            "child" => Self::Child,
            "related" | "relates" => Self::Related,
            "predecessor" | "predecessorof" => Self::Predecessor,
            "successor" | "successorof" => Self::Successor,
            "duplicate" | "duplicateof" => Self::Duplicate,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationRecord {
    pub from: TicketKey,
    pub to: TicketKey,
    pub kind: RelationKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentRecord {
    pub ticket: TicketKey,
    pub comment_id: i64,
    pub created_at: Timestamp,
    pub author: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRecord {
    pub ticket: TicketKey,
    pub revision: i64,
    pub changed_at: Timestamp,
    pub changed_by: Option<String>,
    pub field_name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TicketGraph {
    pub relations: Vec<RelationRecord>,
    pub comments: Vec<CommentRecord>,
    pub history: Vec<HistoryRecord>,
}

/// One work item's discussion and revision history, as a single fetch reads
/// them. Empty vectors are a real answer: plenty of work items have neither.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkItemDetails {
    pub comments: Vec<CommentRecord>,
    pub history: Vec<HistoryRecord>,
}

/// Details read for one work item, and the revision they were read at. That
/// revision is what `work_items.details_rev` records, so a work item edited
/// afterwards is known to need reading again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetailsUpdate {
    pub key: TicketKey,
    pub revision: i64,
    pub details: WorkItemDetails,
}

const MAX_ANCESTOR_DEPTH: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FamilySnapshot {
    pub ancestors: Vec<TicketKey>,
    pub extra_parents: Vec<TicketKey>,
    pub current: TicketKey,
    pub siblings: Vec<TicketKey>,
    pub children: Vec<TicketKey>,
    pub other_links: Vec<(RelationKind, TicketKey)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FamilyTreeEntry {
    pub key: TicketKey,
    pub prefix: String,
    pub is_current: bool,
}

impl TicketGraph {
    #[must_use]
    pub fn relations_from(&self, key: &TicketKey) -> Vec<&RelationRecord> {
        self.relations
            .iter()
            .filter(|relation| relation.from == *key)
            .collect()
    }

    /// Swaps one work item's outgoing links for the set a write brought back,
    /// leaving links from every other work item alone.
    pub fn replace_relations_from(&mut self, key: &TicketKey, relations: Vec<RelationRecord>) {
        self.relations.retain(|relation| relation.from != *key);
        self.relations.extend(relations);
    }

    /// Swaps one work item's comments and history for the set a fetch brought
    /// back, leaving every other work item's alone. This is what keeps a
    /// details fetch from costing a full reload of the graph.
    pub fn replace_details(&mut self, key: &TicketKey, details: WorkItemDetails) {
        self.comments.retain(|comment| comment.ticket != *key);
        self.history.retain(|entry| entry.ticket != *key);
        self.comments.extend(details.comments);
        self.history.extend(details.history);
    }

    #[must_use]
    pub fn family(&self, key: &TicketKey) -> FamilySnapshot {
        FamilySnapshot::from_graph(self, key)
    }

    #[must_use]
    pub fn parents_of(&self, key: &TicketKey) -> Vec<TicketKey> {
        related_keys(self, key, FamilyDirection::Parent)
    }

    #[must_use]
    pub fn children_of(&self, key: &TicketKey) -> Vec<TicketKey> {
        related_keys(self, key, FamilyDirection::Child)
    }

    #[must_use]
    pub fn visible_family_tree(&self, current: &TicketKey) -> Vec<FamilyTreeEntry> {
        let (ancestors, _) = ancestor_chain(self, current);
        let root = ancestors
            .first()
            .cloned()
            .unwrap_or_else(|| current.clone());
        let mut entries = Vec::new();
        let mut path = HashSet::new();
        emit_visible_family(
            self,
            current,
            &root,
            String::from("  "),
            &[],
            &mut path,
            0,
            &mut entries,
        );
        entries
    }

    /// Files one comment just posted, so the details pane shows it without
    /// waiting for the next pull. It lands in `created_at` order, and a comment
    /// already held under the same id is replaced rather than doubled, which is
    /// what keeps a post that raced a details fetch from reading twice.
    pub fn add_comment(&mut self, comment: CommentRecord) {
        self.comments
            .retain(|held| held.ticket != comment.ticket || held.comment_id != comment.comment_id);
        let at = self
            .comments
            .iter()
            .position(|held| held.ticket == comment.ticket && held.created_at > comment.created_at)
            .unwrap_or(self.comments.len());
        self.comments.insert(at, comment);
    }

    #[must_use]
    pub fn comments_for(&self, key: &TicketKey) -> Vec<&CommentRecord> {
        let mut comments: Vec<_> = self
            .comments
            .iter()
            .filter(|comment| comment.ticket == *key)
            .collect();
        comments.sort_by_key(|left| left.created_at);
        comments
    }

    #[must_use]
    pub fn history_for(&self, key: &TicketKey) -> Vec<&HistoryRecord> {
        let mut history: Vec<_> = self
            .history
            .iter()
            .filter(|entry| entry.ticket == *key)
            .collect();
        history.sort_by(|left, right| {
            left.revision
                .cmp(&right.revision)
                .then_with(|| left.changed_at.cmp(&right.changed_at))
                .then_with(|| left.field_name.cmp(&right.field_name))
        });
        history
    }
}

impl FamilySnapshot {
    #[must_use]
    pub fn from_graph(graph: &TicketGraph, key: &TicketKey) -> Self {
        let (ancestors, extra_parents) = ancestor_chain(graph, key);
        let mut siblings = ancestors
            .last()
            .map_or_else(|| vec![key.clone()], |parent| graph.children_of(parent));
        if !siblings.iter().any(|sibling| sibling == key) {
            siblings.push(key.clone());
        }
        sort_keys(&mut siblings);
        Self {
            ancestors,
            extra_parents,
            current: key.clone(),
            siblings,
            children: graph.children_of(key),
            other_links: other_links(graph, key),
        }
    }

    #[must_use]
    pub fn has_family(&self) -> bool {
        !self.ancestors.is_empty()
            || !self.extra_parents.is_empty()
            || !self.children.is_empty()
            || self.siblings.len() > 1
    }

    #[must_use]
    pub fn parent(&self) -> Option<&TicketKey> {
        self.ancestors.last()
    }

    #[must_use]
    pub fn jump_keys(&self) -> Vec<TicketKey> {
        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(self.current.clone());
        let mut push = |key: &TicketKey| {
            if seen.insert(key.clone()) {
                keys.push(key.clone());
            }
        };
        for ancestor in self.ancestors.iter().rev() {
            push(ancestor);
        }
        for parent in &self.extra_parents {
            push(parent);
        }
        for sibling in &self.siblings {
            if sibling != &self.current {
                push(sibling);
            }
        }
        for child in &self.children {
            push(child);
        }
        for (_, key) in &self.other_links {
            push(key);
        }
        keys
    }

    #[must_use]
    pub fn tree_entries(&self) -> Vec<FamilyTreeEntry> {
        let mut entries = Vec::new();
        if !self.has_family() {
            return entries;
        }
        if self.ancestors.is_empty() {
            entries.push(FamilyTreeEntry {
                key: self.current.clone(),
                prefix: "  ".into(),
                is_current: true,
            });
            push_child_entries(&mut entries, &self.children, &[]);
            return entries;
        }

        let mut guides = Vec::new();
        for (index, ancestor) in self.ancestors.iter().enumerate() {
            if index == 0 {
                entries.push(FamilyTreeEntry {
                    key: ancestor.clone(),
                    prefix: "  ".into(),
                    is_current: false,
                });
            } else {
                entries.push(FamilyTreeEntry {
                    key: ancestor.clone(),
                    prefix: tree_prefix(&guides, true),
                    is_current: false,
                });
                guides.push(false);
            }
        }

        for (index, sibling) in self.siblings.iter().enumerate() {
            let is_last = index + 1 == self.siblings.len();
            let is_current = sibling == &self.current;
            entries.push(FamilyTreeEntry {
                key: sibling.clone(),
                prefix: tree_prefix(&guides, is_last),
                is_current,
            });
            if is_current {
                let mut child_guides = guides.clone();
                child_guides.push(!is_last);
                push_child_entries(&mut entries, &self.children, &child_guides);
            }
        }
        entries
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FamilyDirection {
    Parent,
    Child,
}

fn related_keys(
    graph: &TicketGraph,
    key: &TicketKey,
    direction: FamilyDirection,
) -> Vec<TicketKey> {
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    for relation in &graph.relations {
        let other = match direction {
            FamilyDirection::Parent => {
                if relation.from == *key && relation.kind == RelationKind::Parent {
                    Some(&relation.to)
                } else if relation.to == *key && relation.kind == RelationKind::Child {
                    Some(&relation.from)
                } else {
                    None
                }
            }
            FamilyDirection::Child => {
                if relation.from == *key && relation.kind == RelationKind::Child {
                    Some(&relation.to)
                } else if relation.to == *key && relation.kind == RelationKind::Parent {
                    Some(&relation.from)
                } else {
                    None
                }
            }
        };
        let Some(other) = other else {
            continue;
        };
        if other == key {
            continue;
        }
        if seen.insert(other.clone()) {
            keys.push(other.clone());
        }
    }
    sort_keys(&mut keys);
    keys
}

fn ancestor_chain(graph: &TicketGraph, key: &TicketKey) -> (Vec<TicketKey>, Vec<TicketKey>) {
    let mut walk = key.clone();
    let mut seen = HashSet::new();
    seen.insert(key.clone());
    let mut chain = Vec::new();
    let mut extra_parents = Vec::new();
    for depth in 0..MAX_ANCESTOR_DEPTH {
        let mut parents = graph.parents_of(&walk);
        if parents.is_empty() {
            break;
        }
        let primary = parents.remove(0);
        if depth == 0 {
            extra_parents = parents;
        }
        if !seen.insert(primary.clone()) {
            break;
        }
        chain.push(primary.clone());
        walk = primary;
    }
    chain.reverse();
    (chain, extra_parents)
}

fn other_links(graph: &TicketGraph, key: &TicketKey) -> Vec<(RelationKind, TicketKey)> {
    let mut links = Vec::new();
    let mut seen = HashSet::new();
    for relation in graph.relations_from(key) {
        if matches!(relation.kind, RelationKind::Parent | RelationKind::Child) {
            continue;
        }
        if seen.insert((relation.kind, relation.to.clone())) {
            links.push((relation.kind, relation.to.clone()));
        }
    }
    links.sort_by(|left, right| {
        other_link_rank(left.0)
            .cmp(&other_link_rank(right.0))
            .then_with(|| left.1.id.cmp(&right.1.id))
            .then_with(|| left.1.organization.cmp(&right.1.organization))
    });
    links
}

fn other_link_rank(kind: RelationKind) -> u8 {
    match kind {
        RelationKind::Related => 0,
        RelationKind::Predecessor => 1,
        RelationKind::Successor => 2,
        RelationKind::Duplicate => 3,
        RelationKind::Parent | RelationKind::Child => 4,
    }
}

fn sort_keys(keys: &mut [TicketKey]) {
    keys.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.organization.cmp(&right.organization))
    });
}

#[allow(clippy::too_many_arguments)]
fn emit_visible_family(
    graph: &TicketGraph,
    current: &TicketKey,
    key: &TicketKey,
    prefix: String,
    guides: &[bool],
    path: &mut HashSet<TicketKey>,
    depth: usize,
    entries: &mut Vec<FamilyTreeEntry>,
) {
    if depth > MAX_ANCESTOR_DEPTH || path.contains(key) {
        return;
    }

    entries.push(FamilyTreeEntry {
        key: key.clone(),
        prefix,
        is_current: key == current,
    });
    if depth >= MAX_ANCESTOR_DEPTH {
        return;
    }

    path.insert(key.clone());
    let visible_children: Vec<_> = graph
        .children_of(key)
        .into_iter()
        .filter(|child| !path.contains(child))
        .collect();
    for (index, child) in visible_children.iter().enumerate() {
        let is_last = index + 1 == visible_children.len();
        let mut child_guides = guides.to_vec();
        child_guides.push(!is_last);
        emit_visible_family(
            graph,
            current,
            child,
            tree_prefix(guides, is_last),
            &child_guides,
            path,
            depth + 1,
            entries,
        );
    }
    path.remove(key);
}

fn push_child_entries(entries: &mut Vec<FamilyTreeEntry>, children: &[TicketKey], guides: &[bool]) {
    for (index, child) in children.iter().enumerate() {
        let is_last = index + 1 == children.len();
        entries.push(FamilyTreeEntry {
            key: child.clone(),
            prefix: tree_prefix(guides, is_last),
            is_current: false,
        });
    }
}

fn tree_prefix(guides: &[bool], is_last: bool) -> String {
    let mut prefix = String::from("  ");
    for continues in guides {
        prefix.push_str(if *continues { "│ " } else { "  " });
    }
    prefix.push_str(if is_last { "└─" } else { "├─" });
    prefix
}

impl fmt::Display for SortField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchOrder {
    #[default]
    Relevance,
    Field,
}

impl SearchOrder {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Relevance => Self::Field,
            Self::Field => Self::Relevance,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Relevance => "Relevance",
            Self::Field => "Field",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowDensity {
    #[default]
    Compact,
    Comfortable,
}

impl RowDensity {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Compact => Self::Comfortable,
            Self::Comfortable => Self::Compact,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Comfortable => "Comfortable",
        }
    }

    #[must_use]
    pub const fn row_height(self) -> u16 {
        match self {
            Self::Compact => 1,
            Self::Comfortable => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum SortDirection {
    #[serde(rename = "asc")]
    Ascending,
    #[default]
    #[serde(rename = "desc")]
    Descending,
}

impl SortDirection {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Ascending => "↑",
            Self::Descending => "↓",
        }
    }
}

#[must_use]
pub fn compare_tickets(
    left: &Ticket,
    right: &Ticket,
    field: SortField,
    direction: SortDirection,
) -> Ordering {
    let primary = match field {
        SortField::Changed => left.changed_at.cmp(&right.changed_at),
        SortField::Created => left.created_at.cmp(&right.created_at),
        SortField::Priority => compare_optional_last(left.priority, right.priority, direction),
        SortField::Id => left.key.id.cmp(&right.key.id),
        SortField::Title => compare_text(&left.title, &right.title),
        SortField::State => compare_text(&left.state, &right.state),
        SortField::Type => compare_text(&left.work_item_type, &right.work_item_type),
        SortField::Assignee => compare_optional_text_last(
            left.assigned_to.as_deref(),
            right.assigned_to.as_deref(),
            direction,
        ),
        SortField::Organization => compare_text(&left.key.organization, &right.key.organization),
        SortField::Project => compare_text(&left.project, &right.project),
        SortField::Area => compare_text(&left.area_path, &right.area_path),
        SortField::Iteration => compare_text(&left.iteration_path, &right.iteration_path),
        SortField::Tags => compare_text(&left.tags.join(";"), &right.tags.join(";")),
    };

    let directed = if matches!(field, SortField::Priority | SortField::Assignee) {
        primary
    } else {
        apply_direction(primary, direction)
    };

    directed
        .then_with(|| left.key.id.cmp(&right.key.id))
        .then_with(|| left.key.organization.cmp(&right.key.organization))
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}

fn compare_optional_last<T: Ord>(
    left: Option<T>,
    right: Option<T>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => apply_direction(left.cmp(&right), direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_optional_text_last(
    left: Option<&str>,
    right: Option<&str>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => apply_direction(compare_text(left, right), direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn apply_direction(ordering: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => ordering,
        SortDirection::Descending => ordering.reverse(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::ts;

    fn ticket(id: i64, title: &str, priority: Option<i64>) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo-org".into(),
                id,
            },
            project: "demo".into(),
            revision: 1,
            work_item_type: "Task".into(),
            title: title.into(),
            state: "Active".into(),
            reason: None,
            assigned_to: None,
            priority,
            area_path: "demo".into(),
            iteration_path: "demo\\Sprint 1".into(),
            tags: vec!["rust".into()],
            description: "not searchable sentinel".into(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: Timestamp::from_offset_date_time(
                time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(id),
            ),
            web_url: format!("https://dev.azure.com/demo/demo/_workitems/edit/{id}"),
            details_rev: 0,
        }
    }

    #[test]
    fn state_categories_and_path_leaves_normalize_azure_values() {
        assert_eq!(StateCategory::of("To Do"), StateCategory::Proposed);
        assert_eq!(StateCategory::of("  doing "), StateCategory::InProgress);
        assert_eq!(StateCategory::of("Ready for Test"), StateCategory::Resolved);
        assert_eq!(StateCategory::of("DONE"), StateCategory::Completed);
        assert_eq!(StateCategory::of("Removed"), StateCategory::Removed);
        assert_eq!(StateCategory::of("Needs triage"), StateCategory::Unknown);

        assert_eq!(path_leaf("development\\Sprint 1"), "Sprint 1");
        assert_eq!(path_leaf("Atlas/Platform/Web"), "Web");
        assert_eq!(path_leaf("Sprint 1"), "Sprint 1");
        assert_eq!(path_leaf(""), "");
    }

    #[test]
    fn searchable_text_includes_core_fields_but_not_description() {
        let ticket = ticket(42, "Fix search", Some(1));
        let text = ticket.searchable_text();

        assert!(text.contains("42 Fix search"));
        assert!(text.contains("Active Task"));
        assert!(text.contains("Sprint 1"));
        assert!(text.contains("rust"));
        assert!(!text.contains("sentinel"));
    }

    #[test]
    fn priority_sorts_missing_values_last_and_title_sort_ignores_case() {
        let present = ticket(1, "Present", Some(2));
        let missing = ticket(2, "Missing", None);

        assert_eq!(
            compare_tickets(
                &present,
                &missing,
                SortField::Priority,
                SortDirection::Ascending
            ),
            Ordering::Less
        );
        assert_eq!(
            compare_tickets(
                &present,
                &missing,
                SortField::Priority,
                SortDirection::Descending
            ),
            Ordering::Less
        );

        let left = ticket(1, "alpha", Some(1));
        let right = ticket(2, "ALPHA", Some(1));
        assert_eq!(
            compare_tickets(&left, &right, SortField::Title, SortDirection::Descending),
            Ordering::Less,
            "equal titles fall back to the id"
        );
    }

    #[test]
    fn changed_sort_uses_normalized_instants() {
        let mut earlier = ticket(1, "Earlier", Some(1));
        let mut later = ticket(2, "Later", Some(1));
        earlier.changed_at = ts("2026-08-26T16:00:00Z");
        later.changed_at = ts("2026-08-26T13:00:00-05:00");

        assert_eq!(
            compare_tickets(
                &later,
                &earlier,
                SortField::Changed,
                SortDirection::Descending
            ),
            Ordering::Less
        );
    }

    fn key(id: i64) -> TicketKey {
        TicketKey {
            organization: "demo-org".into(),
            id,
        }
    }

    fn relation(from: i64, to: i64, kind: RelationKind) -> RelationRecord {
        RelationRecord {
            from: key(from),
            to: key(to),
            kind,
        }
    }

    #[test]
    fn family_reads_parent_and_child_edges_in_either_direction() {
        let parent_only = TicketGraph {
            relations: vec![
                relation(2, 1, RelationKind::Parent),
                relation(3, 2, RelationKind::Parent),
            ],
            ..TicketGraph::default()
        };
        let child_only = TicketGraph {
            relations: vec![
                relation(1, 2, RelationKind::Child),
                relation(2, 3, RelationKind::Child),
            ],
            ..TicketGraph::default()
        };
        let both = TicketGraph {
            relations: vec![
                relation(2, 1, RelationKind::Parent),
                relation(1, 2, RelationKind::Child),
                relation(3, 2, RelationKind::Parent),
                relation(2, 3, RelationKind::Child),
            ],
            ..TicketGraph::default()
        };

        for graph in [parent_only, child_only, both] {
            let family = graph.family(&key(2));
            assert_eq!(family.ancestors, vec![key(1)]);
            assert_eq!(family.children, vec![key(3)]);
            assert_eq!(graph.family(&key(1)).children, vec![key(2)]);
            assert_eq!(
                ids_of(&graph.visible_family_tree(&key(2))),
                vec![1, 2, 3],
                "either edge direction projects the same tree"
            );
        }
    }

    fn ids_of(entries: &[FamilyTreeEntry]) -> Vec<i64> {
        entries.iter().map(|entry| entry.key.id).collect()
    }

    fn tree_view(entries: &[FamilyTreeEntry]) -> Vec<(i64, &str, bool)> {
        entries
            .iter()
            .map(|entry| (entry.key.id, entry.prefix.as_str(), entry.is_current))
            .collect()
    }

    #[test]
    fn fully_expanded_tree_has_stable_connectors_and_key_order() {
        let graph = TicketGraph {
            relations: vec![
                relation(10, 1, RelationKind::Parent),
                relation(11, 1, RelationKind::Parent),
                relation(12, 1, RelationKind::Parent),
                relation(111, 11, RelationKind::Parent),
                relation(112, 11, RelationKind::Parent),
            ],
            ..TicketGraph::default()
        };
        let first = graph.visible_family_tree(&key(11));
        let second = graph.visible_family_tree(&key(11));

        assert_eq!(
            tree_view(&first),
            vec![
                (1, "  ", false),
                (10, "  ├─", false),
                (11, "  ├─", true),
                (111, "  │ ├─", false),
                (112, "  │ └─", false),
                (12, "  └─", false),
            ],
            "the current ticket nests among its siblings"
        );
        assert_eq!(first, second, "rebuilding the tree gives the same rows");
        assert_eq!(
            graph.family(&key(11)).jump_keys(),
            vec![key(1), key(10), key(12), key(111), key(112)]
        );
    }

    #[test]
    fn primary_parent_stays_in_the_tree_and_extra_parents_stay_out() {
        let graph = TicketGraph {
            relations: vec![
                relation(2, 1, RelationKind::Parent),
                relation(2, 8, RelationKind::Parent),
                relation(3, 2, RelationKind::Parent),
            ],
            ..TicketGraph::default()
        };
        let family = graph.family(&key(2));
        assert_eq!(family.ancestors, vec![key(1)]);
        assert_eq!(family.extra_parents, vec![key(8)]);

        let entries = graph.visible_family_tree(&key(2));
        assert_eq!(ids_of(&entries), vec![1, 2, 3]);
        assert!(!ids_of(&entries).contains(&8));
    }

    #[test]
    fn cycles_and_the_depth_limit_bound_the_family_tree() {
        let graph = TicketGraph {
            relations: vec![
                relation(1, 2, RelationKind::Parent),
                relation(2, 1, RelationKind::Parent),
                relation(1, 9, RelationKind::Related),
            ],
            ..TicketGraph::default()
        };
        let family = graph.family(&key(1));
        let entries = graph.visible_family_tree(&key(1));

        assert_eq!(family.ancestors, vec![key(2)]);
        assert_eq!(
            ids_of(&entries),
            vec![2, 1],
            "the repeating edge is dropped"
        );
        assert!(
            !ids_of(&entries).contains(&9),
            "non-family links stay out of the tree"
        );
        assert_eq!(family.other_links, vec![(RelationKind::Related, key(9))]);
        assert_eq!(family.jump_keys().last(), Some(&key(9)));

        let deep = TicketGraph {
            relations: (1..20)
                .map(|id| relation(id + 1, id, RelationKind::Parent))
                .collect(),
            ..TicketGraph::default()
        };
        let entries = deep.visible_family_tree(&key(20));
        assert!(entries.len() <= MAX_ANCESTOR_DEPTH + 1);
        assert_eq!(entries.last().map(|entry| entry.key.id), Some(20));
        assert!(
            entries
                .iter()
                .all(|entry| entry.prefix.chars().count() < 40)
        );
    }
}
