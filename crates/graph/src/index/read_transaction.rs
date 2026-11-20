use std::ops::Deref;

use rusqlite::{Connection, Transaction};

pub(super) enum ReadTransaction<'a> {
    Owned(Transaction<'a>),
    Borrowed(&'a Connection),
}

impl<'a> ReadTransaction<'a> {
    pub(super) fn new(conn: &'a Connection) -> rusqlite::Result<Self> {
        if conn.is_autocommit() {
            Ok(Self::Owned(conn.unchecked_transaction()?))
        } else {
            Ok(Self::Borrowed(conn))
        }
    }

    pub(super) fn commit(self) -> rusqlite::Result<()> {
        match self {
            Self::Owned(transaction) => transaction.commit(),
            Self::Borrowed(_) => Ok(()),
        }
    }
}

impl Deref for ReadTransaction<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            Self::Owned(transaction) => transaction,
            Self::Borrowed(conn) => conn,
        }
    }
}
