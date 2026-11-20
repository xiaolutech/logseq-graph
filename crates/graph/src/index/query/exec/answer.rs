use super::{QueryExecError, Sub};
use crate::domain::{
    BlockId, BlockSelector, EphemeralId, JournalDate, Priority, QueryBlock, QueryOutcome,
    QueryPage, QueryResult, TaskMarker,
};
use crate::index::{decode_marker, decode_page_kind, IndexError};
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection};

pub(super) fn answer_blocks(
    conn: &Connection,
    plan: &Sub,
    limit: u32,
) -> Result<QueryResult, QueryExecError> {
    let sql = format!(
        "SELECT pages.name, blocks.preorder, blocks.depth, blocks.marker_kind, \
                blocks.heading_level, blocks.text, blocks.task_marker, blocks.priority, \
                blocks.durable_id, blocks.ephemeral_id \
           FROM blocks \
           JOIN pages ON pages.id = blocks.page_id \
          WHERE blocks.id IN ({}) \
          ORDER BY pages.name_key, blocks.preorder \
          LIMIT ?",
        plan.sql
    );
    let mut bounds = plan.params.clone();
    bounds.push(Value::Integer(i64::from(limit) + 1));

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(bounds.iter()))?;
    let mut blocks = Vec::new();
    while let Some(row) = rows.next()? {
        let marker = decode_marker(&row.get::<_, String>(3)?, row.get(4)?)?;
        let task_marker = row
            .get::<_, Option<String>>(6)?
            .map(|stored| decode_task_marker(&stored))
            .transpose()?;
        let priority = row
            .get::<_, Option<String>>(7)?
            .map(|stored| decode_priority(&stored))
            .transpose()?;
        let selector = selector(row.get(8)?, row.get(9)?)?;
        blocks.push(QueryBlock {
            page: row.get(0)?,
            preorder: row.get(1)?,
            depth: row.get(2)?,
            marker,
            text: row.get(5)?,
            task_marker,
            priority,
            selector,
        });
    }
    let truncated = blocks.len() > limit as usize;
    blocks.truncate(limit as usize);
    Ok(QueryResult {
        outcome: QueryOutcome::Blocks(blocks),
        truncated,
    })
}

pub(super) fn answer_pages(
    conn: &Connection,
    plan: &Sub,
    limit: u32,
) -> Result<QueryResult, QueryExecError> {
    let sql = format!(
        "SELECT name, kind, journal_date FROM pages WHERE id IN ({}) \
          ORDER BY name_key LIMIT ?",
        plan.sql
    );
    let mut bounds = plan.params.clone();
    bounds.push(Value::Integer(i64::from(limit) + 1));

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(bounds.iter()))?;
    let mut pages = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: String = row.get(1)?;
        let day: Option<String> = row.get(2)?;
        pages.push(QueryPage {
            name: row.get(0)?,
            kind: decode_page_kind(&kind, "pages.kind")?,
            journal_date: match day.as_deref() {
                Some(stored) => Some(JournalDate::parse_stored(stored).ok_or_else(|| {
                    IndexError::CorruptRow {
                        column: "pages.journal_date",
                        value: stored.to_owned(),
                    }
                })?),
                None => None,
            },
        });
    }
    let truncated = pages.len() > limit as usize;
    pages.truncate(limit as usize);
    Ok(QueryResult {
        outcome: QueryOutcome::Pages(pages),
        truncated,
    })
}

fn decode_task_marker(stored: &str) -> Result<TaskMarker, IndexError> {
    TaskMarker::ALL
        .iter()
        .copied()
        .find(|marker| marker.as_str() == stored)
        .ok_or_else(|| IndexError::CorruptRow {
            column: "blocks.task_marker",
            value: stored.to_owned(),
        })
}

fn decode_priority(stored: &str) -> Result<Priority, IndexError> {
    Priority::ALL
        .iter()
        .copied()
        .find(|priority| priority.as_str() == stored)
        .ok_or_else(|| IndexError::CorruptRow {
            column: "blocks.priority",
            value: stored.to_owned(),
        })
}

/// The selector one block row carries, from the two identity columns it stores.
///
/// The table's CHECK makes exactly one of them non-NULL, so a row with both or with neither is
/// derived state this build cannot read back. Reporting it is what keeps a projection defect from
/// being handed to a caller as a block address that might name the wrong block.
pub(crate) fn selector(
    durable: Option<String>,
    ephemeral: Option<String>,
) -> Result<BlockSelector, IndexError> {
    match (durable, ephemeral) {
        (Some(stored), None) => Ok(BlockSelector::Durable(BlockId::parse(&stored).map_err(
            |_| IndexError::CorruptRow {
                column: "blocks.durable_id",
                value: stored.clone(),
            },
        )?)),
        (None, Some(stored)) => Ok(BlockSelector::Ephemeral(
            EphemeralId::parse(&stored).map_err(|_| IndexError::CorruptRow {
                column: "blocks.ephemeral_id",
                value: stored.clone(),
            })?,
        )),
        (durable, ephemeral) => Err(IndexError::CorruptRow {
            column: "blocks.identity",
            value: format!("durable_id={durable:?} ephemeral_id={ephemeral:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_task_marker_and_priority_decodes_from_its_column() {
        for marker in TaskMarker::ALL {
            assert_eq!(
                decode_task_marker(marker.as_str()).expect("decode"),
                marker,
                "{marker:?} round-trips through the column"
            );
        }
        for priority in Priority::ALL {
            assert_eq!(
                decode_priority(priority.as_str()).expect("decode"),
                priority,
                "{priority:?} round-trips through the column"
            );
        }
        assert!(
            decode_task_marker("todo").is_err(),
            "a value the CHECK refuses"
        );
        assert!(decode_priority("D").is_err());
    }

    #[test]
    fn a_row_states_exactly_one_identity() {
        let durable = Some("01234567-89ab-cdef-0123-456789abcdef".to_owned());
        let ephemeral = Some("01234567-89ab-cdef-0123-456789abcdea".to_owned());
        assert!(matches!(
            selector(durable.clone(), None),
            Ok(BlockSelector::Durable(_))
        ));
        assert!(matches!(
            selector(None, ephemeral.clone()),
            Ok(BlockSelector::Ephemeral(_))
        ));
        assert!(selector(None, None).is_err(), "neither");
        assert!(selector(durable, ephemeral).is_err(), "both");
    }
}
