//! Linking a work item to a branch of a repository.

/// The branch a work item's own work goes on when nobody names one:
/// `{id}-{slug}`, the slug being the title lowercased with every run of
/// characters that are not ASCII letters or digits made one `-`, at most
/// forty characters, so `#715 Fix the thing!` is `715-fix-the-thing`.
#[must_use]
pub fn branch_name(id: i64, title: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        id.to_string()
    } else {
        format!("{id}-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_name_is_the_id_and_a_forty_character_slug() {
        assert_eq!(branch_name(715, "Fix the thing!"), "715-fix-the-thing");
        assert_eq!(
            branch_name(
                715,
                "  Réécrire — the (whole) sync/path, again & again & again "
            ),
            "715-r-crire-the-whole-sync-path-again-again",
            "non-ASCII letters go, runs of punctuation are one dash, and the slug stops at forty"
        );
        assert_eq!(
            branch_name(715, "???"),
            "715",
            "a title with no letters is the id alone"
        );
    }
}
