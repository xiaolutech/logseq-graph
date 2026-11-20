use crate::domain::{list_value_items, page_name_key, PageCandidate, PageMatch};
use crate::index::IndexError;
use rusqlite::{params, Connection};

/// Page names whose folded key contains the query. `name_key` is already [`page_name_key`]'s
/// output, so binding the folded query keeps this comparison on the one page-name rule.
const PAGE_CANDIDATE_NAMES: &str = "\
SELECT name, name_key FROM pages WHERE instr(name_key, ?1) > 0";

/// Every page's last `alias::` value. "Last one wins" is the rule `owners_matching` applies to a
/// repeated property key too, so a query and a lookup answer about the same property line.
const PAGE_CANDIDATE_ALIASES: &str = "\
SELECT pages.name, page_properties.value
  FROM page_properties
  JOIN pages ON pages.id = page_properties.page_id
 WHERE page_properties.normalized_key = 'alias'
   AND page_properties.ordinal = (
       SELECT MAX(later.ordinal) FROM page_properties AS later
        WHERE later.normalized_key = page_properties.normalized_key
          AND later.page_id = page_properties.page_id)";

/// Exact, then prefix, then infix, once both sides are folded page-name keys.
fn name_rank(key: &str, wanted: &str) -> Option<u8> {
    if key == wanted {
        Some(0)
    } else if key.starts_with(wanted) {
        Some(1)
    } else if key.contains(wanted) {
        Some(2)
    } else {
        None
    }
}

/// Pages a name lookup can jump to, ordered so a page's own name outranks an alias.
///
/// Both spellings are first-class: a page answers through its name and through every value of its
/// `alias::` property. A shared alias answers several pages rather than being collapsed, and the
/// caller picks one — the first entry is the default jump, the rest are the alternatives. Rank is
/// exact, then prefix, then infix on the folded [`page_name_key`]; a name hit ranks 0-2 and an
/// alias hit 3-5, so any page-name match sits ahead of any alias match. Within one rank the order
/// is the folded page name, so it does not depend on insertion order (ADR-0002).
pub(in crate::index) fn page_candidates(
    conn: &Connection,
    text: &str,
    limit: u32,
) -> Result<Vec<PageCandidate>, IndexError> {
    if text.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let wanted = page_name_key(text);
    // (rank, page name, how it matched, the alias value that matched)
    let mut found: Vec<(u8, String, PageMatch, Option<String>)> = Vec::new();

    let mut names = conn.prepare(PAGE_CANDIDATE_NAMES)?;
    let mut name_rows = names.query(params![&wanted])?;
    while let Some(row) = name_rows.next()? {
        let name: String = row.get(0)?;
        let key: String = row.get(1)?;
        if let Some(rank) = name_rank(&key, &wanted) {
            found.push((rank, name, PageMatch::Name, None));
        }
    }

    let mut aliases = conn.prepare(PAGE_CANDIDATE_ALIASES)?;
    let mut alias_rows = aliases.query([])?;
    while let Some(row) = alias_rows.next()? {
        let name: String = row.get(0)?;
        let value: String = row.get(1)?;
        let mut best: Option<(u8, String)> = None;
        for item in list_value_items(&value) {
            if let Some(rank) = name_rank(&page_name_key(item), &wanted) {
                let entry = (3 + rank, item.to_owned());
                if best.as_ref().is_none_or(|current| {
                    (entry.0, entry.1.as_str()) < (current.0, current.1.as_str())
                }) {
                    best = Some(entry);
                }
            }
        }
        if let Some((rank, alias)) = best {
            found.push((rank, name, PageMatch::Alias, Some(alias)));
        }
    }

    found.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| page_name_key(&a.1).cmp(&page_name_key(&b.1)))
            .then_with(|| a.1.cmp(&b.1))
    });
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (_, name, via, alias) in found {
        if seen.insert(page_name_key(&name)) {
            out.push(PageCandidate::new(name, via, alias));
            if out.len() == limit as usize {
                break;
            }
        }
    }
    Ok(out)
}
