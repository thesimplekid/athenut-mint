use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

use redb::{Database, ReadableTable, TableDefinition};

const SEARCH_COUNTS_TABLE: TableDefinition<&str, u64> = TableDefinition::new("search_counts_table");

const ALL_TIME_KEY: &str = "all_time_count";

#[derive(Clone)]
pub struct Db {
    inner: Arc<Database>,
}

impl Db {
    pub fn new(path: &PathBuf) -> Result<Self> {
        let db = Arc::new(Database::create(path)?);

        let write_txn = db.begin_write()?;
        {
            let _table = write_txn.open_table(SEARCH_COUNTS_TABLE)?;
        }

        write_txn.commit()?;

        Ok(Self { inner: db })
    }

    pub fn increment_search_count(&self) -> Result<()> {
        let db = &self.inner;

        let write_txn = db.begin_write()?;

        {
            let mut table = write_txn.open_table(SEARCH_COUNTS_TABLE)?;

            let current_all_time = table.get(ALL_TIME_KEY)?.map(|v| v.value()).unwrap_or(0);
            let new_all_time = current_all_time + 1;
            table.insert(ALL_TIME_KEY, new_all_time)?;
        }

        write_txn.commit()?;

        Ok(())
    }

    pub fn get_search_count(&self) -> Result<SearchCount> {
        let db = &self.inner;

        let read_txn = db.begin_read()?;

        let table = read_txn.open_table(SEARCH_COUNTS_TABLE)?;

        let current_all_time = table.get(ALL_TIME_KEY)?.map(|v| v.value()).unwrap_or(0);

        Ok(SearchCount {
            all_time_search_count: current_all_time,
        })
    }
}

#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize)]
pub struct SearchCount {
    pub all_time_search_count: u64,
}
