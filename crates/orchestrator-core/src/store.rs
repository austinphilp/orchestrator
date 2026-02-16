use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;

use crate::error::CoreError;
use crate::events::{NewEventEnvelope, OrchestrationEventPayload, StoredEventEnvelope};
use crate::identifiers::{ArtifactId, TicketId, TicketProvider, WorkItemId, WorkerSessionId};
use crate::status::ArtifactKind;

const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalScope {
    Global,
    WorkItem(WorkItemId),
    Session(WorkerSessionId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketRecord {
    pub ticket_id: TicketId,
    pub provider: TicketProvider,
    pub provider_ticket_id: String,
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketWorkItemMapping {
    pub ticket_id: TicketId,
    pub work_item_id: WorkItemId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRecord {
    pub artifact_id: ArtifactId,
    pub work_item_id: WorkItemId,
    pub kind: ArtifactKind,
    pub metadata: Value,
    pub storage_ref: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEventWithArtifacts {
    pub event: StoredEventEnvelope,
    pub artifact_ids: Vec<ArtifactId>,
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
        let mut store = Self { conn };
        store.bootstrap()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        let conn =
            Connection::open_in_memory().map_err(|err| CoreError::Persistence(err.to_string()))?;
        let mut store = Self { conn };
        store.bootstrap()?;
        Ok(store)
    }

    pub fn schema_version(&self) -> Result<u32, CoreError> {
        self.current_schema_version()
    }

    pub fn append_event(
        &mut self,
        event: NewEventEnvelope,
        artifact_refs: &[ArtifactId],
    ) -> Result<StoredEventEnvelope, CoreError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let stored = Self::append_event_tx(&tx, event, artifact_refs)?;
        tx.commit()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        Ok(stored)
    }

    pub fn read_events_for_work_item(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Vec<StoredEventEnvelope>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                WHERE work_item_id = ?1
                ORDER BY sequence ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map(params![work_item_id.as_str()], |row| {
                Self::map_row(row).map_err(to_from_sql_error)
            })
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn read_event_with_artifacts(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Vec<StoredEventWithArtifacts>, CoreError> {
        let events = self.read_events_for_work_item(work_item_id)?;
        events
            .into_iter()
            .map(|event| {
                self.read_artifact_refs_for_event(&event.event_id)
                    .map(|artifact_ids| StoredEventWithArtifacts {
                        event,
                        artifact_ids,
                    })
            })
            .collect()
    }

    pub fn upsert_ticket(&self, ticket: &TicketRecord) -> Result<(), CoreError> {
        self.conn
            .execute(
                "
                INSERT INTO tickets (
                    ticket_id, provider, provider_ticket_id, identifier, title, state, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(ticket_id) DO UPDATE SET
                    provider = excluded.provider,
                    provider_ticket_id = excluded.provider_ticket_id,
                    identifier = excluded.identifier,
                    title = excluded.title,
                    state = excluded.state,
                    updated_at = excluded.updated_at
                ",
                params![
                    ticket.ticket_id.as_str(),
                    provider_to_str(&ticket.provider),
                    ticket.provider_ticket_id,
                    ticket.identifier,
                    ticket.title,
                    ticket.state,
                    ticket.updated_at,
                ],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn map_ticket_to_work_item(
        &self,
        mapping: &TicketWorkItemMapping,
    ) -> Result<(), CoreError> {
        self.conn
            .execute(
                "
                INSERT INTO work_items (work_item_id, ticket_id, created_at)
                VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                ON CONFLICT(work_item_id) DO UPDATE SET
                    ticket_id = excluded.ticket_id
                ",
                params![mapping.work_item_id.as_str(), mapping.ticket_id.as_str()],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn find_work_item_by_ticket(
        &self,
        provider: &TicketProvider,
        provider_ticket_id: &str,
    ) -> Result<Option<WorkItemId>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT wi.work_item_id
                FROM work_items wi
                JOIN tickets t ON t.ticket_id = wi.ticket_id
                WHERE t.provider = ?1 AND t.provider_ticket_id = ?2
                ",
                params![provider_to_str(provider), provider_ticket_id],
                |row| row.get::<_, String>(0).map(WorkItemId::from),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn find_ticket_by_work_item(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<TicketRecord>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT t.ticket_id, t.provider, t.provider_ticket_id, t.identifier, t.title, t.state, t.updated_at
                FROM tickets t
                JOIN work_items wi ON wi.ticket_id = t.ticket_id
                WHERE wi.work_item_id = ?1
                ",
                params![work_item_id.as_str()],
                |row| {
                    Ok(TicketRecord {
                        ticket_id: row.get::<_, String>(0)?.into(),
                        provider: str_to_provider(&row.get::<_, String>(1)?)?,
                        provider_ticket_id: row.get(2)?,
                        identifier: row.get(3)?,
                        title: row.get(4)?,
                        state: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn create_artifact(&self, artifact: &ArtifactRecord) -> Result<(), CoreError> {
        let metadata_json = serde_json::to_string(&artifact.metadata)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        self.conn
            .execute(
                "
                INSERT INTO artifacts (
                    artifact_id, work_item_id, kind, metadata_json, storage_ref, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ",
                params![
                    artifact.artifact_id.as_str(),
                    artifact.work_item_id.as_str(),
                    serde_json::to_string(&artifact.kind)
                        .map_err(|err| CoreError::Persistence(err.to_string()))?,
                    metadata_json,
                    artifact.storage_ref,
                    artifact.created_at,
                ],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn get_artifact(
        &self,
        artifact_id: &ArtifactId,
    ) -> Result<Option<ArtifactRecord>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT artifact_id, work_item_id, kind, metadata_json, storage_ref, created_at
                FROM artifacts
                WHERE artifact_id = ?1
                ",
                params![artifact_id.as_str()],
                |row| {
                    let metadata: String = row.get(3)?;
                    let kind: String = row.get(2)?;
                    Ok(ArtifactRecord {
                        artifact_id: row.get::<_, String>(0)?.into(),
                        work_item_id: row.get::<_, String>(1)?.into(),
                        kind: serde_json::from_str(&kind).map_err(to_from_sql_error)?,
                        metadata: serde_json::from_str(&metadata).map_err(to_from_sql_error)?,
                        storage_ref: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn bootstrap(&mut self) -> Result<(), CoreError> {
        self.conn
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let current = self.current_schema_version()?;
        if current > CURRENT_SCHEMA_VERSION {
            return Err(CoreError::UnsupportedSchemaVersion {
                supported: CURRENT_SCHEMA_VERSION,
                found: current,
            });
        }

        self.apply_pending_migrations(current)
    }

    fn table_exists(&self, name: &str) -> Result<bool, CoreError> {
        self.conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
                params![name],
                |_| Ok(()),
            )
            .optional()
            .map(|opt| opt.is_some())
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn current_schema_version(&self) -> Result<u32, CoreError> {
        if !self.table_exists("schema_migrations")? {
            return Ok(0);
        }

        self.conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn apply_pending_migrations(&mut self, current: u32) -> Result<(), CoreError> {
        for version in (current + 1)..=CURRENT_SCHEMA_VERSION {
            let tx = self
                .conn
                .transaction()
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            Self::apply_migration(&tx, version)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                params![version],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
            tx.commit()
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
        }

        Ok(())
    }

    fn apply_migration(tx: &Transaction<'_>, version: u32) -> Result<(), CoreError> {
        match version {
            1 => tx
                .execute_batch(
                    "
                    CREATE TABLE schema_migrations (
                        version INTEGER PRIMARY KEY,
                        applied_at TEXT NOT NULL
                    );

                    CREATE TABLE tickets (
                        ticket_id TEXT PRIMARY KEY,
                        provider TEXT NOT NULL,
                        provider_ticket_id TEXT NOT NULL,
                        identifier TEXT NOT NULL,
                        title TEXT NOT NULL,
                        state TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        UNIQUE(provider, provider_ticket_id)
                    );

                    CREATE TABLE work_items (
                        work_item_id TEXT PRIMARY KEY,
                        ticket_id TEXT NOT NULL UNIQUE,
                        created_at TEXT NOT NULL,
                        FOREIGN KEY(ticket_id) REFERENCES tickets(ticket_id)
                    );

                    CREATE TABLE artifacts (
                        artifact_id TEXT PRIMARY KEY,
                        work_item_id TEXT NOT NULL,
                        kind TEXT NOT NULL,
                        metadata_json TEXT NOT NULL,
                        storage_ref TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        FOREIGN KEY(work_item_id) REFERENCES work_items(work_item_id)
                    );

                    CREATE TABLE events (
                        event_id TEXT PRIMARY KEY,
                        sequence INTEGER NOT NULL UNIQUE,
                        occurred_at TEXT NOT NULL,
                        work_item_id TEXT,
                        session_id TEXT,
                        event_type TEXT NOT NULL,
                        payload TEXT NOT NULL,
                        schema_version INTEGER NOT NULL
                    );

                    CREATE TABLE event_artifact_refs (
                        event_id TEXT NOT NULL,
                        artifact_id TEXT NOT NULL,
                        PRIMARY KEY (event_id, artifact_id),
                        FOREIGN KEY(event_id) REFERENCES events(event_id),
                        FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                    );

                    CREATE INDEX idx_events_work_item_sequence ON events(work_item_id, sequence ASC);
                    CREATE INDEX idx_tickets_provider_lookup ON tickets(provider, provider_ticket_id);
                    CREATE INDEX idx_event_artifact_refs_event ON event_artifact_refs(event_id);
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            _ => Err(CoreError::Persistence(format!(
                "no migration implementation for version {version}"
            ))),
        }
    }

    fn append_event_tx(
        tx: &Transaction<'_>,
        event: NewEventEnvelope,
        artifact_refs: &[ArtifactId],
    ) -> Result<StoredEventEnvelope, CoreError> {
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

        for artifact_id in artifact_refs {
            tx.execute(
                "INSERT INTO event_artifact_refs (event_id, artifact_id) VALUES (?1, ?2)",
                params![event.event_id, artifact_id.as_str()],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        }

        Ok((sequence, event).into())
    }

    fn read_artifact_refs_for_event(&self, event_id: &str) -> Result<Vec<ArtifactId>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT artifact_id
                FROM event_artifact_refs
                WHERE event_id = ?1
                ORDER BY artifact_id ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map(params![event_id], |row| {
                row.get::<_, String>(0).map(ArtifactId::from)
            })
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
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
        self.append_event(event, &[])
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
            .query_map([], |row| Self::map_row(row).map_err(to_from_sql_error))
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Persistence(err.to_string()))
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

        if let Some(value) = bind {
            let rows = stmt
                .query_map(params![value, limit], |row| {
                    Self::map_row(row).map_err(to_from_sql_error)
                })
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|err| CoreError::Persistence(err.to_string()))
        } else {
            let rows = stmt
                .query_map(params![limit], |row| {
                    Self::map_row(row).map_err(to_from_sql_error)
                })
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|err| CoreError::Persistence(err.to_string()))
        }
    }
}

fn provider_to_str(provider: &TicketProvider) -> &'static str {
    match provider {
        TicketProvider::Linear => "linear",
    }
}

fn str_to_provider(value: &str) -> Result<TicketProvider, rusqlite::Error> {
    match value {
        "linear" => Ok(TicketProvider::Linear),
        other => Err(to_from_sql_error(std::io::Error::other(format!(
            "unknown ticket provider: {other}"
        )))),
    }
}

fn to_from_sql_error<E>(err: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}
