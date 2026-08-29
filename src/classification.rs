//! The project's classification nodes: the iteration and area trees a work
//! item's `System.IterationPath` and `System.AreaPath` point into.
//!
//! Azure DevOps returns both trees from one endpoint, nested, each node
//! carrying a `path` of its own — `\development\Iteration\Sprint 1`. That is
//! not the value a work item holds: the field reads `development\Sprint 1`,
//! the project root followed by the descendants, with the `Iteration` or
//! `Area` segment that only exists to separate the two trees left out. So the
//! field path is built here, from the names on the way down, rather than taken
//! from what the server says.

use serde_json::Value;
use time::Date;

use crate::model::path_leaf;
use crate::timestamp::Timestamp;

/// Which of the two trees a node belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NodeKind {
    Area,
    Iteration,
}

impl NodeKind {
    /// The `structureType` Azure DevOps labels the tree with, which is also how
    /// the kind is stored.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Area => "area",
            Self::Iteration => "iteration",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "area" => Some(Self::Area),
            "iteration" => Some(Self::Iteration),
            _ => None,
        }
    }

    /// What a picker and a notification call the field, such as `Iteration`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Area => "Area",
            Self::Iteration => "Iteration",
        }
    }
}

/// One node of one tree, flattened: the field path a work item would carry, how
/// deep it sits, and — for an iteration that has been scheduled — the days it
/// runs between.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassificationNode {
    pub kind: NodeKind,
    /// The value the work item's field holds, such as `development\Sprint 1`.
    pub path: String,
    /// Zero for the project root, one for its children, and so on. The picker
    /// indents by this.
    pub depth: usize,
    pub start_date: Option<Timestamp>,
    pub finish_date: Option<Timestamp>,
}

impl ClassificationNode {
    #[must_use]
    pub fn new(kind: NodeKind, path: impl Into<String>, depth: usize) -> Self {
        Self {
            kind,
            path: path.into(),
            depth,
            start_date: None,
            finish_date: None,
        }
    }

    /// The last segment of the path, which is what the row and the table column
    /// show: `development\Sprint 1` reads as `Sprint 1`.
    #[must_use]
    pub fn leaf(&self) -> &str {
        path_leaf(&self.path)
    }

    /// Whether `today` falls inside this node's dates. A node missing either
    /// end contains nothing: an iteration nobody scheduled is never current.
    /// Both ends are inclusive, compared by calendar day, so a sprint finishing
    /// on the 5th is still current all through the 5th.
    #[must_use]
    pub fn contains(&self, today: Date) -> bool {
        matches!(
            (self.start_date, self.finish_date),
            (Some(start), Some(finish)) if start.date() <= today && today <= finish.date()
        )
    }

    /// How long the node runs for, in seconds, and zero when it has no
    /// schedule. This only ever separates two nodes that both contain today.
    #[must_use]
    fn span_seconds(&self) -> i64 {
        self.start_date
            .zip(self.finish_date)
            .map_or(0, |(start, finish)| start.seconds_until(finish))
    }

    /// The days the node runs between, as a picker row shows them, such as
    /// `Aug 25 \u{2013} Sep 5`. Nothing at all when it has no schedule.
    #[must_use]
    pub fn date_range(&self) -> Option<String> {
        let (start, finish) = self.start_date.zip(self.finish_date)?;
        Some(format!(
            "{} \u{2013} {}",
            start.calendar_day(),
            finish.calendar_day()
        ))
    }
}

/// The deepest iteration whose dates contain `today`, which is the sprint the
/// project is in: a nested `development\Q3\Sprint 7` wins over the `Q3` that
/// spans it. Two at the same depth are separated by the shorter span, so a
/// two-week sprint wins over a quarter kept beside it. Nothing at all when no
/// iteration is scheduled around today.
#[must_use]
pub fn current_iteration(nodes: &[ClassificationNode], today: Date) -> Option<&ClassificationNode> {
    nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Iteration && node.contains(today))
        .max_by_key(|node| (node.depth, -node.span_seconds()))
}

/// Both trees, flattened in the order Azure DevOps nests them: each root
/// followed by its descendants, depth first.
#[must_use]
pub fn parse_classification_nodes(response: &Value) -> Vec<ClassificationNode> {
    let mut nodes = Vec::new();
    let Some(roots) = response.get("value").and_then(Value::as_array) else {
        return nodes;
    };
    for root in roots {
        let Some(kind) = root
            .get("structureType")
            .and_then(Value::as_str)
            .and_then(NodeKind::parse)
        else {
            continue;
        };
        collect(kind, root, "", &mut nodes);
    }
    nodes
}

/// Walks one node and everything under it, building the field path from the
/// names on the way down rather than from the `path` the server sends.
fn collect(kind: NodeKind, node: &Value, prefix: &str, nodes: &mut Vec<ClassificationNode>) {
    let Some(name) = node
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return;
    };
    let path = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}\\{name}")
    };
    let depth = path.matches('\\').count();
    let attribute = |field: &str| {
        node.get("attributes")
            .and_then(|attributes| attributes.get(field))
            .and_then(Value::as_str)
            .and_then(|raw| Timestamp::parse(raw).ok())
    };
    nodes.push(ClassificationNode {
        kind,
        path: path.clone(),
        depth,
        start_date: attribute("startDate"),
        finish_date: attribute("finishDate"),
    });
    for child in node
        .get("children")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
    {
        collect(kind, child, &path, nodes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::ts;
    use serde_json::json;
    use time::macros::date;

    fn trees() -> Value {
        json!({
            "count": 2,
            "value": [
                {
                    "name": "development",
                    "structureType": "area",
                    "path": "\\development\\Area",
                    "children": [
                        {
                            "name": "Platform",
                            "structureType": "area",
                            "path": "\\development\\Area\\Platform",
                        },
                    ],
                },
                {
                    "name": "development",
                    "structureType": "iteration",
                    "path": "\\development\\Iteration",
                    "children": [
                        {
                            "name": "Sprint 1",
                            "structureType": "iteration",
                            "path": "\\development\\Iteration\\Sprint 1",
                            "attributes": {
                                "startDate": "2026-08-25T00:00:00Z",
                                "finishDate": "2026-09-05T00:00:00Z",
                            },
                        },
                        {
                            "name": "Q3",
                            "structureType": "iteration",
                            "path": "\\development\\Iteration\\Q3",
                            "attributes": {
                                "startDate": "2026-07-01T00:00:00Z",
                                "finishDate": "2026-09-30T00:00:00Z",
                            },
                            "children": [
                                {
                                    "name": "Sprint 7",
                                    "structureType": "iteration",
                                    "path": "\\development\\Iteration\\Q3\\Sprint 7",
                                    "attributes": {
                                        "startDate": "2026-09-07T00:00:00Z",
                                        "finishDate": "2026-09-18T00:00:00Z",
                                    },
                                },
                            ],
                        },
                    ],
                },
            ],
        })
    }

    #[test]
    fn a_tree_flattens_to_field_paths_built_from_the_names_on_the_way_down() {
        let nodes = parse_classification_nodes(&trees());
        let rows: Vec<(&str, &str, usize)> = nodes
            .iter()
            .map(|node| (node.kind.as_str(), node.path.as_str(), node.depth))
            .collect();

        assert_eq!(
            rows,
            [
                ("area", "development", 0),
                ("area", "development\\Platform", 1),
                ("iteration", "development", 0),
                ("iteration", "development\\Sprint 1", 1),
                ("iteration", "development\\Q3", 1),
                ("iteration", "development\\Q3\\Sprint 7", 2),
            ],
            "the Iteration and Area segments the server's own path carries are not field paths"
        );
        assert_eq!(nodes[5].leaf(), "Sprint 7");
        assert_eq!(
            nodes[3].date_range().as_deref(),
            Some("Aug 25 \u{2013} Sep 5")
        );
        assert_eq!(nodes[0].date_range(), None, "an area has no schedule");
    }

    #[test]
    fn the_current_iteration_is_the_deepest_one_containing_today() {
        let nodes = parse_classification_nodes(&trees());

        assert_eq!(
            current_iteration(&nodes, date!(2026 - 09 - 10)).map(|node| node.path.as_str()),
            Some("development\\Q3\\Sprint 7"),
            "a nested sprint wins over the quarter that spans it"
        );
        assert_eq!(
            current_iteration(&nodes, date!(2026 - 09 - 05)).map(|node| node.path.as_str()),
            Some("development\\Sprint 1"),
            "a sprint runs through the whole of its finish day, and beats the quarter \
             it shares a depth with by being the shorter of the two"
        );
        assert_eq!(
            current_iteration(&nodes, date!(2026 - 09 - 30)).map(|node| node.path.as_str()),
            Some("development\\Q3")
        );
        assert_eq!(
            current_iteration(&nodes, date!(2026 - 12 - 25)),
            None,
            "no iteration is scheduled around Christmas"
        );

        let undated = vec![ClassificationNode::new(
            NodeKind::Iteration,
            "development\\Sprint 1",
            1,
        )];
        assert_eq!(
            current_iteration(&undated, date!(2026 - 09 - 10)),
            None,
            "an iteration nobody scheduled is never current"
        );
    }

    #[test]
    fn a_response_missing_its_pieces_yields_what_it_can() {
        assert!(parse_classification_nodes(&json!({})).is_empty());
        assert!(
            parse_classification_nodes(&json!({"value": [{"name": "development"}]})).is_empty(),
            "a tree with no structure type belongs to neither picker"
        );
        assert!(
            parse_classification_nodes(&json!({
                "value": [{"structureType": "iteration", "name": "  "}]
            }))
            .is_empty(),
            "a node with no name has no field path"
        );

        let partial = parse_classification_nodes(&json!({
            "value": [{
                "structureType": "iteration",
                "name": "development",
                "children": [{
                    "structureType": "iteration",
                    "name": "Sprint 1",
                    "attributes": {"startDate": "2026-08-25T00:00:00Z"},
                }],
            }],
        }));
        assert_eq!(partial.len(), 2);
        assert_eq!(partial[1].start_date, Some(ts("2026-08-25T00:00:00Z")));
        assert_eq!(partial[1].finish_date, None);
        assert_eq!(
            partial[1].date_range(),
            None,
            "half a schedule is not a range"
        );
        assert!(!partial[1].contains(date!(2026 - 08 - 26)));
    }
}
