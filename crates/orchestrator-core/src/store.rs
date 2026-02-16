use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;

use crate::adapters::BackendKind;
use crate::error::CoreError;
use crate::events::{NewEventEnvelope, OrchestrationEventPayload, StoredEventEnvelope};
use crate::identifiers::{ArtifactId, TicketId, TicketProvider, WorkItemId, WorkerSessionId};
use crate::status::{ArtifactKind, WorkerSessionStatus};

const CURRENT_SCHEMA_VERSION: u32 = 2;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub worktree_id: crate::identifiers::WorktreeId,
    pub work_item_id: WorkItemId,
    pub path: String,
    pub branch: String,
    pub base_branch: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: WorkerSessionId,
    pub work_item_id: WorkItemId,
    pub backend_kind: BackendKind,
    pub workdir: String,
    pub model: Option<String>,
    pub status: WorkerSessionStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMappingRecord {
    pub ticket: TicketRecord,
    pub work_item_id: WorkItemId,
    pub worktree: WorktreeRecord,
    pub session: SessionRecord,
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
        validate_ticket_identity(ticket)?;

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
        self.ensure_ticket_work_item_pairing(&mapping.ticket_id, &mapping.work_item_id)?;

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

    pub fn upsert_worktree(&self, worktree: &WorktreeRecord) -> Result<(), CoreError> {
        self.conn
            .execute(
                "
                INSERT INTO worktrees (
                    worktree_id, work_item_id, path, branch, base_branch, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(work_item_id) DO UPDATE SET
                    worktree_id = excluded.worktree_id,
                    work_item_id = excluded.work_item_id,
                    path = excluded.path,
                    branch = excluded.branch,
                    base_branch = excluded.base_branch,
                    created_at = excluded.created_at
                ",
                params![
                    worktree.worktree_id.as_str(),
                    worktree.work_item_id.as_str(),
                    worktree.path,
                    worktree.branch,
                    worktree.base_branch,
                    worktree.created_at,
                ],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn find_worktree_by_work_item(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<WorktreeRecord>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT worktree_id, work_item_id, path, branch, base_branch, created_at
                FROM worktrees
                WHERE work_item_id = ?1
                ",
                params![work_item_id.as_str()],
                |row| {
                    Ok(WorktreeRecord {
                        worktree_id: row.get::<_, String>(0)?.into(),
                        work_item_id: row.get::<_, String>(1)?.into(),
                        path: row.get(2)?,
                        branch: row.get(3)?,
                        base_branch: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn upsert_session(&self, session: &SessionRecord) -> Result<(), CoreError> {
        self.conn
            .execute(
                "
                INSERT INTO sessions (
                    session_id, work_item_id, backend_kind, workdir, model, status, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(work_item_id) DO UPDATE SET
                    session_id = excluded.session_id,
                    work_item_id = excluded.work_item_id,
                    backend_kind = excluded.backend_kind,
                    workdir = excluded.workdir,
                    model = excluded.model,
                    status = excluded.status,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at
                ",
                params![
                    session.session_id.as_str(),
                    session.work_item_id.as_str(),
                    backend_kind_to_json(&session.backend_kind)?,
                    session.workdir,
                    session.model,
                    session_status_to_json(&session.status)?,
                    session.created_at,
                    session.updated_at,
                ],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn find_session_by_work_item(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<SessionRecord>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT session_id, work_item_id, backend_kind, workdir, model, status, created_at, updated_at
                FROM sessions
                WHERE work_item_id = ?1
                ",
                params![work_item_id.as_str()],
                |row| {
                    Ok(SessionRecord {
                        session_id: row.get::<_, String>(0)?.into(),
                        work_item_id: row.get::<_, String>(1)?.into(),
                        backend_kind: str_to_backend_kind(&row.get::<_, String>(2)?)?,
                        workdir: row.get(3)?,
                        model: row.get(4)?,
                        status: str_to_session_status(&row.get::<_, String>(5)?)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn upsert_runtime_mapping(
        &mut self,
        mapping: &RuntimeMappingRecord,
    ) -> Result<(), CoreError> {
        validate_ticket_identity(&mapping.ticket)?;

        if mapping.work_item_id != mapping.worktree.work_item_id {
            return Err(CoreError::Persistence(
                "runtime mapping worktree.work_item_id does not match work_item_id".to_owned(),
            ));
        }
        if mapping.work_item_id != mapping.session.work_item_id {
            return Err(CoreError::Persistence(
                "runtime mapping session.work_item_id does not match work_item_id".to_owned(),
            ));
        }
        if mapping.worktree.path != mapping.session.workdir {
            return Err(CoreError::Persistence(
                "runtime mapping session.workdir does not match worktree.path".to_owned(),
            ));
        }

        self.ensure_ticket_work_item_pairing(&mapping.ticket.ticket_id, &mapping.work_item_id)?;

        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Self::upsert_ticket_tx(&tx, &mapping.ticket)?;
        Self::map_ticket_to_work_item_tx(
            &tx,
            &TicketWorkItemMapping {
                ticket_id: mapping.ticket.ticket_id.clone(),
                work_item_id: mapping.work_item_id.clone(),
            },
        )?;
        Self::upsert_worktree_tx(&tx, &mapping.worktree)?;
        Self::upsert_session_tx(&tx, &mapping.session)?;

        tx.commit()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn find_runtime_mapping_by_ticket(
        &self,
        provider: &TicketProvider,
        provider_ticket_id: &str,
    ) -> Result<Option<RuntimeMappingRecord>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT
                    t.ticket_id,
                    t.provider,
                    t.provider_ticket_id,
                    t.identifier,
                    t.title,
                    t.state,
                    t.updated_at,
                    wi.work_item_id,
                    wt.worktree_id,
                    wt.path,
                    wt.branch,
                    wt.base_branch,
                    wt.created_at,
                    s.session_id,
                    s.backend_kind,
                    s.workdir,
                    s.model,
                    s.status,
                    s.created_at,
                    s.updated_at
                FROM work_items wi
                JOIN tickets t ON t.ticket_id = wi.ticket_id
                JOIN worktrees wt ON wt.work_item_id = wi.work_item_id
                JOIN sessions s ON s.work_item_id = wi.work_item_id
                WHERE t.provider = ?1 AND t.provider_ticket_id = ?2
                ",
                params![provider_to_str(provider), provider_ticket_id],
                |row| Self::map_runtime_mapping_row(row),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn find_inflight_runtime_mapping_by_ticket(
        &self,
        provider: &TicketProvider,
        provider_ticket_id: &str,
    ) -> Result<Option<RuntimeMappingRecord>, CoreError> {
        let running = session_status_to_json(&WorkerSessionStatus::Running)?;
        let waiting_for_user = session_status_to_json(&WorkerSessionStatus::WaitingForUser)?;
        let blocked = session_status_to_json(&WorkerSessionStatus::Blocked)?;

        self.conn
            .query_row(
                "
                SELECT
                    t.ticket_id,
                    t.provider,
                    t.provider_ticket_id,
                    t.identifier,
                    t.title,
                    t.state,
                    t.updated_at,
                    wi.work_item_id,
                    wt.worktree_id,
                    wt.path,
                    wt.branch,
                    wt.base_branch,
                    wt.created_at,
                    s.session_id,
                    s.backend_kind,
                    s.workdir,
                    s.model,
                    s.status,
                    s.created_at,
                    s.updated_at
                FROM work_items wi
                JOIN tickets t ON t.ticket_id = wi.ticket_id
                JOIN worktrees wt ON wt.work_item_id = wi.work_item_id
                JOIN sessions s ON s.work_item_id = wi.work_item_id
                WHERE t.provider = ?1
                  AND t.provider_ticket_id = ?2
                  AND s.status IN (?3, ?4, ?5)
                ",
                params![
                    provider_to_str(provider),
                    provider_ticket_id,
                    running,
                    waiting_for_user,
                    blocked
                ],
                |row| Self::map_runtime_mapping_row(row),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn list_runtime_mappings(&self) -> Result<Vec<RuntimeMappingRecord>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT
                    t.ticket_id,
                    t.provider,
                    t.provider_ticket_id,
                    t.identifier,
                    t.title,
                    t.state,
                    t.updated_at,
                    wi.work_item_id,
                    wt.worktree_id,
                    wt.path,
                    wt.branch,
                    wt.base_branch,
                    wt.created_at,
                    s.session_id,
                    s.backend_kind,
                    s.workdir,
                    s.model,
                    s.status,
                    s.created_at,
                    s.updated_at
                FROM work_items wi
                JOIN tickets t ON t.ticket_id = wi.ticket_id
                JOIN worktrees wt ON wt.work_item_id = wi.work_item_id
                JOIN sessions s ON s.work_item_id = wi.work_item_id
                ORDER BY t.identifier ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map([], |row| Self::map_runtime_mapping_row(row))
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn list_inflight_runtime_mappings(&self) -> Result<Vec<RuntimeMappingRecord>, CoreError> {
        let running = session_status_to_json(&WorkerSessionStatus::Running)?;
        let waiting_for_user = session_status_to_json(&WorkerSessionStatus::WaitingForUser)?;
        let blocked = session_status_to_json(&WorkerSessionStatus::Blocked)?;

        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT
                    t.ticket_id,
                    t.provider,
                    t.provider_ticket_id,
                    t.identifier,
                    t.title,
                    t.state,
                    t.updated_at,
                    wi.work_item_id,
                    wt.worktree_id,
                    wt.path,
                    wt.branch,
                    wt.base_branch,
                    wt.created_at,
                    s.session_id,
                    s.backend_kind,
                    s.workdir,
                    s.model,
                    s.status,
                    s.created_at,
                    s.updated_at
                FROM work_items wi
                JOIN tickets t ON t.ticket_id = wi.ticket_id
                JOIN worktrees wt ON wt.work_item_id = wi.work_item_id
                JOIN sessions s ON s.work_item_id = wi.work_item_id
                WHERE s.status IN (?1, ?2, ?3)
                ORDER BY t.identifier ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map(params![running, waiting_for_user, blocked], |row| {
                Self::map_runtime_mapping_row(row)
            })
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Persistence(err.to_string()))
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

        self.adopt_legacy_schema_v1_if_needed()?;

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

    fn adopt_legacy_schema_v1_if_needed(&self) -> Result<(), CoreError> {
        if self.table_exists("schema_migrations")? || !self.table_exists("events")? {
            return Ok(());
        }

        self.conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS tickets (
                    ticket_id TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    provider_ticket_id TEXT NOT NULL,
                    identifier TEXT NOT NULL,
                    title TEXT NOT NULL,
                    state TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(provider, provider_ticket_id)
                );

                CREATE TABLE IF NOT EXISTS work_items (
                    work_item_id TEXT PRIMARY KEY,
                    ticket_id TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY(ticket_id) REFERENCES tickets(ticket_id)
                );

                CREATE TABLE IF NOT EXISTS artifacts (
                    artifact_id TEXT PRIMARY KEY,
                    work_item_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    metadata_json TEXT NOT NULL,
                    storage_ref TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY(work_item_id) REFERENCES work_items(work_item_id)
                );

                CREATE TABLE IF NOT EXISTS worktrees (
                    worktree_id TEXT PRIMARY KEY,
                    work_item_id TEXT NOT NULL UNIQUE,
                    path TEXT NOT NULL,
                    branch TEXT NOT NULL,
                    base_branch TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY(work_item_id) REFERENCES work_items(work_item_id)
                );

                CREATE TABLE IF NOT EXISTS sessions (
                    session_id TEXT PRIMARY KEY,
                    work_item_id TEXT NOT NULL UNIQUE,
                    backend_kind TEXT NOT NULL,
                    workdir TEXT NOT NULL,
                    model TEXT,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY(work_item_id) REFERENCES work_items(work_item_id)
                );

                CREATE TABLE IF NOT EXISTS event_artifact_refs (
                    event_id TEXT NOT NULL,
                    artifact_id TEXT NOT NULL,
                    PRIMARY KEY (event_id, artifact_id),
                    FOREIGN KEY(event_id) REFERENCES events(event_id),
                    FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                );

                CREATE INDEX IF NOT EXISTS idx_events_work_item_sequence ON events(work_item_id, sequence ASC);
                CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id, sequence DESC);
                CREATE INDEX IF NOT EXISTS idx_tickets_provider_lookup ON tickets(provider, provider_ticket_id);
                CREATE INDEX IF NOT EXISTS idx_event_artifact_refs_event ON event_artifact_refs(event_id);
                CREATE INDEX IF NOT EXISTS idx_worktrees_work_item_lookup ON worktrees(work_item_id);
                CREATE INDEX IF NOT EXISTS idx_sessions_work_item_lookup ON sessions(work_item_id);
                CREATE INDEX IF NOT EXISTS idx_sessions_status_lookup ON sessions(status);
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        self.conn
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) ON CONFLICT(version) DO NOTHING",
                params![CURRENT_SCHEMA_VERSION],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
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
                    CREATE INDEX idx_events_session ON events(session_id, sequence DESC);
                    CREATE INDEX idx_tickets_provider_lookup ON tickets(provider, provider_ticket_id);
                    CREATE INDEX idx_event_artifact_refs_event ON event_artifact_refs(event_id);
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            2 => tx
                .execute_batch(
                    "
                    CREATE TABLE worktrees (
                        worktree_id TEXT PRIMARY KEY,
                        work_item_id TEXT NOT NULL UNIQUE,
                        path TEXT NOT NULL,
                        branch TEXT NOT NULL,
                        base_branch TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        FOREIGN KEY(work_item_id) REFERENCES work_items(work_item_id)
                    );

                    CREATE TABLE sessions (
                        session_id TEXT PRIMARY KEY,
                        work_item_id TEXT NOT NULL UNIQUE,
                        backend_kind TEXT NOT NULL,
                        workdir TEXT NOT NULL,
                        model TEXT,
                        status TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        FOREIGN KEY(work_item_id) REFERENCES work_items(work_item_id)
                    );

                    CREATE INDEX idx_worktrees_work_item_lookup ON worktrees(work_item_id);
                    CREATE INDEX idx_sessions_work_item_lookup ON sessions(work_item_id);
                    CREATE INDEX idx_sessions_status_lookup ON sessions(status);
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            _ => Err(CoreError::Persistence(format!(
                "no migration implementation for version {version}"
            ))),
        }
    }

    fn ensure_ticket_work_item_pairing(
        &self,
        ticket_id: &TicketId,
        work_item_id: &WorkItemId,
    ) -> Result<(), CoreError> {
        if let Some(existing_work_item_id) = self.find_work_item_id_by_ticket_id(ticket_id)? {
            if existing_work_item_id != *work_item_id {
                return Err(CoreError::Persistence(format!(
                    "ticket_id '{}' is already mapped to work_item_id '{}'",
                    ticket_id.as_str(),
                    existing_work_item_id.as_str()
                )));
            }
        }

        if let Some(existing_ticket_id) = self.find_ticket_id_by_work_item_id(work_item_id)? {
            if existing_ticket_id != *ticket_id {
                return Err(CoreError::Persistence(format!(
                    "work_item_id '{}' is already mapped to ticket_id '{}'",
                    work_item_id.as_str(),
                    existing_ticket_id.as_str()
                )));
            }
        }

        Ok(())
    }

    fn find_work_item_id_by_ticket_id(
        &self,
        ticket_id: &TicketId,
    ) -> Result<Option<WorkItemId>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT work_item_id
                FROM work_items
                WHERE ticket_id = ?1
                ",
                params![ticket_id.as_str()],
                |row| row.get::<_, String>(0).map(WorkItemId::from),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn find_ticket_id_by_work_item_id(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<TicketId>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT ticket_id
                FROM work_items
                WHERE work_item_id = ?1
                ",
                params![work_item_id.as_str()],
                |row| row.get::<_, String>(0).map(TicketId::from),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn upsert_ticket_tx(tx: &Transaction<'_>, ticket: &TicketRecord) -> Result<(), CoreError> {
        validate_ticket_identity(ticket)?;

        tx.execute(
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

    fn map_ticket_to_work_item_tx(
        tx: &Transaction<'_>,
        mapping: &TicketWorkItemMapping,
    ) -> Result<(), CoreError> {
        tx.execute(
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

    fn upsert_worktree_tx(
        tx: &Transaction<'_>,
        worktree: &WorktreeRecord,
    ) -> Result<(), CoreError> {
        tx.execute(
            "
            INSERT INTO worktrees (
                worktree_id, work_item_id, path, branch, base_branch, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(work_item_id) DO UPDATE SET
                worktree_id = excluded.worktree_id,
                work_item_id = excluded.work_item_id,
                path = excluded.path,
                branch = excluded.branch,
                base_branch = excluded.base_branch,
                created_at = excluded.created_at
            ",
            params![
                worktree.worktree_id.as_str(),
                worktree.work_item_id.as_str(),
                worktree.path,
                worktree.branch,
                worktree.base_branch,
                worktree.created_at,
            ],
        )
        .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    fn upsert_session_tx(tx: &Transaction<'_>, session: &SessionRecord) -> Result<(), CoreError> {
        tx.execute(
            "
            INSERT INTO sessions (
                session_id, work_item_id, backend_kind, workdir, model, status, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(work_item_id) DO UPDATE SET
                session_id = excluded.session_id,
                work_item_id = excluded.work_item_id,
                backend_kind = excluded.backend_kind,
                workdir = excluded.workdir,
                model = excluded.model,
                status = excluded.status,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at
            ",
            params![
                session.session_id.as_str(),
                session.work_item_id.as_str(),
                backend_kind_to_json(&session.backend_kind)?,
                session.workdir,
                session.model,
                session_status_to_json(&session.status)?,
                session.created_at,
                session.updated_at,
            ],
        )
        .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    fn map_runtime_mapping_row(
        row: &rusqlite::Row<'_>,
    ) -> Result<RuntimeMappingRecord, rusqlite::Error> {
        let ticket = TicketRecord {
            ticket_id: row.get::<_, String>(0)?.into(),
            provider: str_to_provider(&row.get::<_, String>(1)?)?,
            provider_ticket_id: row.get(2)?,
            identifier: row.get(3)?,
            title: row.get(4)?,
            state: row.get(5)?,
            updated_at: row.get(6)?,
        };
        let work_item_id: WorkItemId = row.get::<_, String>(7)?.into();

        Ok(RuntimeMappingRecord {
            ticket,
            work_item_id: work_item_id.clone(),
            worktree: WorktreeRecord {
                worktree_id: row.get::<_, String>(8)?.into(),
                work_item_id: work_item_id.clone(),
                path: row.get(9)?,
                branch: row.get(10)?,
                base_branch: row.get(11)?,
                created_at: row.get(12)?,
            },
            session: SessionRecord {
                session_id: row.get::<_, String>(13)?.into(),
                work_item_id,
                backend_kind: str_to_backend_kind(&row.get::<_, String>(14)?)?,
                workdir: row.get(15)?,
                model: row.get(16)?,
                status: str_to_session_status(&row.get::<_, String>(17)?)?,
                created_at: row.get(18)?,
                updated_at: row.get(19)?,
            },
        })
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

fn validate_ticket_identity(ticket: &TicketRecord) -> Result<(), CoreError> {
    let expected =
        TicketId::from_provider_uuid(ticket.provider.clone(), &ticket.provider_ticket_id);
    if ticket.ticket_id == expected {
        return Ok(());
    }

    Err(CoreError::Persistence(format!(
        "ticket_id '{}' does not match canonical id '{}' for provider '{}' and provider_ticket_id '{}'",
        ticket.ticket_id.as_str(),
        expected.as_str(),
        provider_to_str(&ticket.provider),
        ticket.provider_ticket_id
    )))
}

fn backend_kind_to_json(kind: &BackendKind) -> Result<String, CoreError> {
    serde_json::to_string(kind).map_err(|err| CoreError::Persistence(err.to_string()))
}

fn str_to_backend_kind(value: &str) -> Result<BackendKind, rusqlite::Error> {
    serde_json::from_str(value).map_err(to_from_sql_error)
}

fn session_status_to_json(status: &WorkerSessionStatus) -> Result<String, CoreError> {
    serde_json::to_string(status).map_err(|err| CoreError::Persistence(err.to_string()))
}

fn str_to_session_status(value: &str) -> Result<WorkerSessionStatus, rusqlite::Error> {
    serde_json::from_str(value).map_err(to_from_sql_error)
}

fn to_from_sql_error<E>(err: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}
