use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::CoreError;
use crate::events::{NewEventEnvelope, OrchestrationEventPayload, StoredEventEnvelope};
use crate::identifiers::{WorkItemId, WorkerSessionId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalScope {
    Global,
    WorkItem(WorkItemId),
    Session(WorkerSessionId),
}

pub trait EventStore {
    fn append(&mut self, event: NewEventEnvelope) -> Result<StoredEventEnvelope, CoreError>;
    fn read_ordered(&self) -> Result<Vec<StoredEventEnvelope>, CoreError>;
    fn query(
        &self,
        scope: RetrievalScope,
        limit: usize,
    ) -> Result<Vec<StoredEventEnvelope>, CoreError>;
}

pub struct SqliteEventStore {
    conn: Connection,
}

impl SqliteEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let conn = Connection::open(path).map_err(|err| CoreError::Persistence(err.to_string()))?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        let conn = Connection::open_in_memory()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), CoreError> {
        self.conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS events (
                    event_id TEXT PRIMARY KEY,
                    sequence INTEGER NOT NULL UNIQUE,
                    occurred_at TEXT NOT NULL,
                    work_item_id TEXT NULL,
                    session_id TEXT NULL,
                    event_type TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    schema_version INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_events_sequence ON events(sequence);
                CREATE INDEX IF NOT EXISTS idx_events_work_item ON events(work_item_id, sequence DESC);
                CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id, sequence DESC);
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn map_row(row: &rusqlite::Row<'_>) -> Result<StoredEventEnvelope, CoreError> {
        let payload_json: String = row
            .get(6)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let payload: OrchestrationEventPayload = serde_json::from_str(&payload_json)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(StoredEventEnvelope {
            event_id: row
                .get(0)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
            sequence: row
                .get(1)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
            occurred_at: row
                .get(2)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
            work_item_id: row
                .get::<_, Option<String>>(3)
                .map_err(|err| CoreError::Persistence(err.to_string()))?
                .map(WorkItemId::from),
            session_id: row
                .get::<_, Option<String>>(4)
                .map_err(|err| CoreError::Persistence(err.to_string()))?
                .map(WorkerSessionId::from),
            event_type: serde_json::from_str(
                &row.get::<_, String>(5)
                    .map_err(|err| CoreError::Persistence(err.to_string()))?,
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?,
            payload,
            schema_version: row
                .get(7)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
        })
    }
}

impl EventStore for SqliteEventStore {
    fn append(&mut self, event: NewEventEnvelope) -> Result<StoredEventEnvelope, CoreError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let max_sequence: Option<u64> = tx
            .query_row("SELECT MAX(sequence) FROM events", [], |row| row.get(0))
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))?
            .flatten();
        let sequence = max_sequence.unwrap_or(0) + 1;

        let payload_json = serde_json::to_string(&event.payload)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let event_type = serde_json::to_string(&event.payload.event_type())
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let work_item_id = event.work_item_id.as_ref().map(|id| id.as_str().to_owned());
        let session_id = event.session_id.as_ref().map(|id| id.as_str().to_owned());

        tx.execute(
            "
            INSERT INTO events (
                event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                event.event_id,
                sequence,
                event.occurred_at,
                work_item_id,
                session_id,
                event_type,
                payload_json,
                event.schema_version,
            ],
        )
        .map_err(|err| CoreError::Persistence(err.to_string()))?;
        tx.commit()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok((sequence, event).into())
    }

    fn read_ordered(&self) -> Result<Vec<StoredEventEnvelope>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                ORDER BY sequence ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                Self::map_row(row).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other(err.to_string())),
                    )
                })
            })
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|err| CoreError::Persistence(err.to_string()))?);
        }

        Ok(result)
    }

    fn query(
        &self,
        scope: RetrievalScope,
        limit: usize,
    ) -> Result<Vec<StoredEventEnvelope>, CoreError> {
        if limit == 0 {
            return Ok(vec![]);
        }

        let (sql, bind): (&str, Option<String>) = match scope {
            RetrievalScope::Global => (
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                ORDER BY sequence DESC
                LIMIT ?1
                ",
                None,
            ),
            RetrievalScope::WorkItem(id) => (
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                WHERE work_item_id = ?1
                ORDER BY sequence DESC
                LIMIT ?2
                ",
                Some(id.as_str().to_owned()),
            ),
            RetrievalScope::Session(id) => (
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                WHERE session_id = ?1
                ORDER BY sequence DESC
                LIMIT ?2
                ",
                Some(id.as_str().to_owned()),
            ),
        };

        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut result = Vec::new();
        if let Some(value) = bind {
            let rows = stmt
                .query_map(params![value, limit], |row| {
                    Self::map_row(row).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::other(err.to_string())),
                        )
                    })
                })
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            for row in rows {
                result.push(row.map_err(|err| CoreError::Persistence(err.to_string()))?);
            }
        } else {
            let rows = stmt
                .query_map(params![limit], |row| {
                    Self::map_row(row).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::other(err.to_string())),
                        )
                    })
                })
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            for row in rows {
                result.push(row.map_err(|err| CoreError::Persistence(err.to_string()))?);
            }
        }

        Ok(result)
    }
}
