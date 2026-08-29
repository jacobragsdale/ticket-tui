use crate::model::Ticket;

#[must_use]
pub fn copy_ids(tickets: &[&Ticket]) -> String {
    join_lines(tickets.iter().map(|ticket| ticket.key.id.to_string()))
}

#[must_use]
pub fn copy_urls(tickets: &[&Ticket]) -> String {
    join_lines(tickets.iter().map(|ticket| ticket.web_url.clone()))
}

#[must_use]
pub fn copy_titles(tickets: &[&Ticket]) -> String {
    join_lines(tickets.iter().map(|ticket| ticket.title.clone()))
}

#[must_use]
pub fn copy_markdown_links(tickets: &[&Ticket]) -> String {
    join_lines(
        tickets
            .iter()
            .map(|ticket| format!("[{}]({})", ticket.title, ticket.web_url)),
    )
}

#[must_use]
pub fn copy_summaries(tickets: &[&Ticket]) -> String {
    join_lines(tickets.iter().map(|ticket| {
        format!(
            "{} {} [{}] {} · {}",
            ticket.key.id,
            ticket.title,
            ticket.work_item_type,
            ticket.state,
            ticket.assigned_to.as_deref().unwrap_or("Unassigned")
        )
    }))
}

#[must_use]
pub fn export_json(tickets: &[&Ticket]) -> String {
    let rows: Vec<_> = tickets
        .iter()
        .map(|ticket| {
            serde_json::json!({
                "organization": ticket.key.organization,
                "id": ticket.key.id,
                "project": ticket.project,
                "type": ticket.work_item_type,
                "title": ticket.title,
                "state": ticket.state,
                "assignee": ticket.assigned_to,
                "priority": ticket.priority,
                "area": ticket.area_path,
                "iteration": ticket.iteration_path,
                "tags": ticket.tags,
                "created_at": ticket.created_at.to_rfc3339(),
                "changed_at": ticket.changed_at.to_rfc3339(),
                "url": ticket.web_url,
            })
        })
        .collect();
    serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into())
}

#[must_use]
pub fn export_csv(tickets: &[&Ticket]) -> String {
    let mut output = String::from(
        "id,organization,project,type,title,state,assignee,priority,area,iteration,tags,url\n",
    );
    for ticket in tickets {
        output.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            ticket.key.id,
            csv_field(&ticket.key.organization),
            csv_field(&ticket.project),
            csv_field(&ticket.work_item_type),
            csv_field(&ticket.title),
            csv_field(&ticket.state),
            csv_field(ticket.assigned_to.as_deref().unwrap_or("")),
            ticket
                .priority
                .map_or_else(String::new, |priority| priority.to_string()),
            csv_field(&ticket.area_path),
            csv_field(&ticket.iteration_path),
            csv_field(&ticket.tags.join(";")),
            csv_field(&ticket.web_url),
        ));
    }
    output
}

fn join_lines(lines: impl Iterator<Item = String>) -> String {
    let mut output = lines.collect::<Vec<_>>().join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    output
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TicketKey;

    fn ticket() -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id: 42,
            },
            project: "atlas".into(),
            revision: 1,
            work_item_type: "Bug".into(),
            title: "Fix, please".into(),
            state: "Active".into(),
            reason: None,
            assigned_to: Some("Avery Chen".into()),
            priority: Some(1),
            area_path: "Atlas\\Platform".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: vec!["rust".into(), "search".into()],
            description: String::new(),
            created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
            changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
            web_url: "https://dev.azure.com/demo/atlas/_workitems/edit/42".into(),
        }
    }

    #[test]
    fn copy_helpers_join_tickets_and_csv_and_json_escape_fields() {
        let ticket = ticket();
        let tickets = [&ticket];

        assert_eq!(copy_ids(&tickets), "42\n");
        assert!(copy_markdown_links(&tickets).contains("[Fix, please]("));
        assert!(copy_summaries(&tickets).contains("42 Fix, please"));

        let csv = export_csv(&tickets);
        let json = export_json(&tickets);
        assert!(csv.contains("\"Fix, please\""));
        assert!(json.contains("\"id\": 42"));
        assert!(json.contains("Fix, please"));
    }
}
