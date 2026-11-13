//! What a sortable, searchable, pageable table remembers between requests.

use std::collections::BTreeMap;

use url::form_urlencoded;

pub const PAGE_SIZE: i64 = 100;

/// A column a table can be sorted by.
#[derive(Clone, Copy)]
pub struct SortChoice {
    pub key: &'static str,
    pub label: &'static str,
    /// Whether the first click sorts upwards.
    pub asc_first: bool,
}

pub struct Column {
    pub label: &'static str,
    pub href: String,
    pub aria_sort: &'static str,
    pub arrow: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct TableState {
    pub path: &'static str,
    pub filters: BTreeMap<&'static str, String>,
    pub search: String,
    pub sort: &'static str,
    pub asc: bool,
    /// The key of the last row shown; the page starts after it.
    pub after: String,
}

impl TableState {
    /// Reads `q`, `sort`, `dir` and `after`; anything unrecognised falls back
    /// to the first sort choice.
    #[must_use]
    pub fn parse(
        path: &'static str,
        params: &BTreeMap<String, String>,
        choices: &[SortChoice],
    ) -> Self {
        let first = choices[0];
        let mut state = Self {
            path,
            filters: BTreeMap::new(),
            search: params.get("q").map_or_default(|q| q.trim().to_owned()),
            sort: first.key,
            asc: first.asc_first,
            after: String::new(),
        };
        if let Some(choice) = choices
            .iter()
            .find(|c| params.get("sort").is_some_and(|s| s == c.key))
        {
            state.sort = choice.key;
            state.asc = choice.asc_first;
        }
        match params.get("dir").map(String::as_str) {
            Some("asc") => state.asc = true,
            Some("desc") => state.asc = false,
            _ => {}
        }
        if let Some(after) = params.get("after") {
            state.after.clone_from(after);
        }
        state
    }

    #[must_use]
    pub fn dir(&self) -> &'static str {
        if self.asc { "asc" } else { "desc" }
    }

    fn href(&self, sort: &str, asc: bool, after: &str) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (name, value) in self.filters.iter().filter(|(_, v)| !v.is_empty()) {
            query.append_pair(name, value);
        }
        if !self.search.is_empty() {
            query.append_pair("q", &self.search);
        }
        query.append_pair("sort", sort);
        query.append_pair("dir", if asc { "asc" } else { "desc" });
        if !after.is_empty() {
            query.append_pair("after", after);
        }
        format!("{}?{}", self.path, query.finish())
    }

    /// Header links: the sorted column flips direction, the others start in
    /// their natural one.
    #[must_use]
    pub fn columns(&self, choices: &[SortChoice]) -> Vec<Column> {
        choices
            .iter()
            .map(|choice| {
                if choice.key == self.sort {
                    Column {
                        label: choice.label,
                        href: self.href(choice.key, !self.asc, ""),
                        aria_sort: if self.asc { "ascending" } else { "descending" },
                        arrow: if self.asc { "▲" } else { "▼" },
                    }
                } else {
                    Column {
                        label: choice.label,
                        href: self.href(choice.key, choice.asc_first, ""),
                        aria_sort: "none",
                        arrow: "",
                    }
                }
            })
            .collect()
    }

    /// The link that loads the page after the row keyed `last`, or empty when
    /// there is none.
    #[must_use]
    pub fn more_href(&self, last: &str, more: bool) -> String {
        if more {
            self.href(self.sort, self.asc, last)
        } else {
            String::new()
        }
    }
}

#[must_use]
pub fn count_label(total: i64, one: &str, many: &str, filtered: bool) -> String {
    let noun = if total == 1 { one } else { many };
    match (filtered, total) {
        (false, _) => format!("{total} {noun}"),
        (true, 1) => format!("{total} {noun} matches"),
        (true, _) => format!("{total} {noun} match"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHOICES: [SortChoice; 2] = [
        SortChoice {
            key: "time",
            label: "Time",
            asc_first: false,
        },
        SortChoice {
            key: "from",
            label: "From",
            asc_first: true,
        },
    ];

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn defaults_to_the_first_choice() {
        let state = TableState::parse("/", &params(&[]), &CHOICES);

        assert_eq!(
            (state.sort, state.asc, state.after.as_str()),
            ("time", false, "")
        );
    }

    #[test]
    fn a_known_sort_starts_in_its_natural_direction_unless_told() {
        let natural = TableState::parse("/", &params(&[("sort", "from")]), &CHOICES);
        let told = TableState::parse("/", &params(&[("sort", "from"), ("dir", "desc")]), &CHOICES);
        let unknown = TableState::parse("/", &params(&[("sort", "bogus")]), &CHOICES);

        assert_eq!((natural.sort, natural.asc), ("from", true));
        assert_eq!((told.sort, told.asc), ("from", false));
        assert_eq!(unknown.sort, "time");
    }

    #[test]
    fn the_sorted_column_flips_and_links_keep_the_filters() {
        let mut state =
            TableState::parse("/", &params(&[("q", " spam "), ("sort", "from")]), &CHOICES);
        state.filters.insert("verdict", "good".to_owned());

        let columns = state.columns(&CHOICES);

        assert_eq!(columns[0].href, "/?verdict=good&q=spam&sort=time&dir=desc");
        assert_eq!((columns[1].aria_sort, columns[1].arrow), ("ascending", "▲"));
        assert_eq!(columns[1].href, "/?verdict=good&q=spam&sort=from&dir=desc");
    }

    #[test]
    fn the_next_page_link_names_the_last_row_shown() {
        let state = TableState::parse("/", &params(&[("after", "a+b@x.org")]), &CHOICES);

        assert_eq!(state.after, "a+b@x.org");
        assert_eq!(state.more_href("a+b@x.org", false), "");
        assert_eq!(
            state.more_href("a+b@x.org", true),
            "/?sort=time&dir=desc&after=a%2Bb%40x.org"
        );
    }

    #[test]
    fn counts_agree_with_their_noun() {
        assert_eq!(count_label(1, "address", "addresses", false), "1 address");
        assert_eq!(
            count_label(1, "address", "addresses", true),
            "1 address matches"
        );
        assert_eq!(
            count_label(2, "address", "addresses", true),
            "2 addresses match"
        );
    }
}
