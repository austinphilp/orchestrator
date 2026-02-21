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

    pub fn count_events(&self, scope: RetrievalScope) -> Result<usize, CoreError> {
        let count: i64 = match scope {
            RetrievalScope::Global => {
                self.conn
                    .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            }
            RetrievalScope::WorkItem(id) => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE work_item_id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            ),
            RetrievalScope::Session(id) => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            ),
        }
        .map_err(|err| CoreError::Persistence(err.to_string()))?;

        usize::try_from(count).map_err(|_| {
            CoreError::Persistence(format!(
                "event count '{count}' cannot be represented as usize"
            ))
        })
    }

    pub fn prune_completed_session_events(
        &mut self,
        policy: EventPrunePolicy,
        now_unix_seconds: u64,
    ) -> Result<EventPruneReport, CoreError> {
        if policy.retention_days == 0 {
            return Err(CoreError::Configuration(
                "event prune retention_days must be greater than zero".to_owned(),
            ));
        }

        let retention_window_seconds = policy.retention_days.saturating_mul(86_400);
        let cutoff_unix_seconds = now_unix_seconds.saturating_sub(retention_window_seconds);
        let done = session_status_to_json(&WorkerSessionStatus::Done)?;
        let crashed = session_status_to_json(&WorkerSessionStatus::Crashed)?;

        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut report = EventPruneReport {
            cutoff_unix_seconds,
            ..EventPruneReport::default()
        };

        let mut stmt = tx
            .prepare(
                "
                SELECT session_id, work_item_id, updated_at
                FROM sessions
                WHERE status IN (?1, ?2)
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map(params![done, crashed], |row| {
                let session_id: String = row.get(0)?;
                let work_item_id: String = row.get(1)?;
                let updated_at: String = row.get(2)?;
                Ok((session_id, work_item_id, updated_at))
            })
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut eligible = Vec::new();
        for row in rows {
            let (session_id, work_item_id, updated_at) =
                row.map_err(|err| CoreError::Persistence(err.to_string()))?;
            report.candidate_sessions = report.candidate_sessions.saturating_add(1);

            let Some(updated_at_unix_seconds) = parse_unix_seconds(updated_at.as_str()) else {
                report.skipped_invalid_timestamps =
                    report.skipped_invalid_timestamps.saturating_add(1);
                continue;
            };

            if updated_at_unix_seconds <= cutoff_unix_seconds {
                eligible.push((session_id, work_item_id));
            }
        }
        drop(stmt);

        report.eligible_sessions = u64::try_from(eligible.len()).map_err(|_| {
            CoreError::Persistence("eligible session count exceeds u64".to_owned())
        })?;

        for (session_id, work_item_id) in eligible {
            let deleted_event_artifact_refs = tx
                .execute(
                    "
                    DELETE FROM event_artifact_refs
                    WHERE event_id IN (
                        SELECT event_id
                        FROM events
                        WHERE work_item_id = ?1 OR session_id = ?2
                    )
                    ",
                    params![work_item_id.as_str(), session_id.as_str()],
                )
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            report.deleted_event_artifact_refs = report
                .deleted_event_artifact_refs
                .saturating_add(u64::try_from(deleted_event_artifact_refs).map_err(|_| {
                    CoreError::Persistence(
                        "deleted event-artifact reference count exceeds u64".to_owned(),
                    )
                })?);

            let deleted_events = tx
                .execute(
                    "DELETE FROM events WHERE work_item_id = ?1 OR session_id = ?2",
                    params![work_item_id.as_str(), session_id.as_str()],
                )
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            report.deleted_events = report
                .deleted_events
                .saturating_add(u64::try_from(deleted_events).map_err(|_| {
                    CoreError::Persistence("deleted event count exceeds u64".to_owned())
                })?);
            if deleted_events > 0 {
                report.pruned_work_items = report.pruned_work_items.saturating_add(1);
            }
        }

        tx.commit()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(report)
    }

    pub fn read_event_with_artifacts(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Vec<StoredEventWithArtifacts>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT
                    e.event_id,
                    e.sequence,
                    e.occurred_at,
                    e.work_item_id,
                    e.session_id,
                    e.event_type,
                    e.payload,
                    e.schema_version,
                    r.artifact_id
                FROM events e
                LEFT JOIN event_artifact_refs r ON r.event_id = e.event_id
                WHERE e.work_item_id = ?1
                ORDER BY e.sequence ASC, r.rowid ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut rows = stmt
            .query(params![work_item_id.as_str()])
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut output = Vec::<StoredEventWithArtifacts>::new();
        let mut current_event: Option<StoredEventEnvelope> = None;
        let mut current_artifact_ids = Vec::<ArtifactId>::new();

        while let Some(row) = rows
            .next()
            .map_err(|err| CoreError::Persistence(err.to_string()))?
        {
            let event = Self::map_row(row)?;
            let artifact_id = row
                .get::<_, Option<String>>(8)
                .map_err(|err| CoreError::Persistence(err.to_string()))?
                .map(ArtifactId::from);

            let same_event = current_event
                .as_ref()
                .map(|existing| existing.event_id == event.event_id)
                .unwrap_or(false);

            if !same_event {
                if let Some(previous_event) = current_event.take() {
                    output.push(StoredEventWithArtifacts {
                        event: previous_event,
                        artifact_ids: std::mem::take(&mut current_artifact_ids),
                    });
                }
                current_event = Some(event);
            }

            if let Some(artifact_id) = artifact_id {
                current_artifact_ids.push(artifact_id);
            }
        }

        if let Some(previous_event) = current_event.take() {
            output.push(StoredEventWithArtifacts {
                event: previous_event,
                artifact_ids: current_artifact_ids,
            });
        }

        Ok(output)
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
        self.ensure_worktree_binding_consistency(worktree)?;

        if let Some(session) = self.find_session_by_work_item(&worktree.work_item_id)? {
            if session.workdir != worktree.path {
                return Err(CoreError::Persistence(format!(
                    "worktree path '{}' does not match existing session workdir '{}' for work_item_id '{}'",
                    worktree.path,
                    session.workdir,
                    worktree.work_item_id.as_str()
                )));
            }
        }

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
        self.ensure_session_binding_consistency(session)?;

        if let Some(worktree) = self.find_worktree_by_work_item(&session.work_item_id)? {
            if worktree.path != session.workdir {
                return Err(CoreError::Persistence(format!(
                    "session workdir '{}' does not match existing worktree path '{}' for work_item_id '{}'",
                    session.workdir,
                    worktree.path,
                    session.work_item_id.as_str()
                )));
            }
        }

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

        self.conn
            .execute(
                "
                INSERT INTO session_runtime_flags (session_id, is_working, updated_at)
                VALUES (?1, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                ON CONFLICT(session_id) DO NOTHING
                ",
                params![session.session_id.as_str()],
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

    pub fn set_session_working_state(
        &self,
        session_id: &WorkerSessionId,
        is_working: bool,
    ) -> Result<(), CoreError> {
        let working = if is_working { 1_i64 } else { 0_i64 };
        self.conn
            .execute(
                "
                INSERT OR IGNORE INTO session_runtime_flags (session_id, is_working, updated_at)
                SELECT session_id, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                FROM sessions
                WHERE session_id = ?1
                ",
                params![session_id.as_str(), working],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        self.conn
            .execute(
                "
                UPDATE session_runtime_flags
                SET is_working = ?2,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                WHERE session_id = ?1
                ",
                params![session_id.as_str(), working],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        Ok(())
    }

    pub fn is_session_working(&self, session_id: &WorkerSessionId) -> Result<bool, CoreError> {
        let working = self
            .conn
            .query_row(
                "
                SELECT COALESCE((
                    SELECT is_working
                    FROM session_runtime_flags
                    WHERE session_id = ?1
                ), 0)
                ",
                params![session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        Ok(working != 0)
    }

    pub fn list_session_working_states(
        &self,
    ) -> Result<std::collections::HashMap<WorkerSessionId, bool>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT s.session_id, COALESCE(f.is_working, 0)
                FROM sessions s
                LEFT JOIN session_runtime_flags f
                    ON f.session_id = s.session_id
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let session_id = WorkerSessionId::new(row.get::<_, String>(0)?);
                let is_working = row.get::<_, i64>(1)? != 0;
                Ok((session_id, is_working))
            })
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut states = std::collections::HashMap::new();
        for row in rows {
            let (session_id, is_working) =
                row.map_err(|err| CoreError::Persistence(err.to_string()))?;
            states.insert(session_id, is_working);
        }
        Ok(states)
    }

    pub fn upsert_harness_session_binding(
        &self,
        session_id: &WorkerSessionId,
        backend_kind: &BackendKind,
        harness_session_id: &str,
    ) -> Result<(), CoreError> {
        let harness_session_id = harness_session_id.trim();
        if harness_session_id.is_empty() {
            return Err(CoreError::Configuration(
                "harness session id cannot be empty".to_owned(),
            ));
        }

        self.conn
            .execute(
                "
                INSERT INTO harness_session_bindings (
                    session_id, backend_kind, harness_session_id, updated_at
                ) VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                ON CONFLICT(session_id, backend_kind) DO UPDATE SET
                    harness_session_id = excluded.harness_session_id,
                    updated_at = excluded.updated_at
                ",
                params![
                    session_id.as_str(),
                    backend_kind_to_json(backend_kind)?,
                    harness_session_id
                ],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn find_harness_session_binding(
        &self,
        session_id: &WorkerSessionId,
        backend_kind: &BackendKind,
    ) -> Result<Option<String>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT harness_session_id
                FROM harness_session_bindings
                WHERE session_id = ?1
                  AND backend_kind = ?2
                ",
                params![session_id.as_str(), backend_kind_to_json(backend_kind)?],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn delete_harness_session_binding(
        &self,
        session_id: &WorkerSessionId,
        backend_kind: &BackendKind,
    ) -> Result<(), CoreError> {
        self.conn
            .execute(
                "
                DELETE FROM harness_session_bindings
                WHERE session_id = ?1
                  AND backend_kind = ?2
                ",
                params![session_id.as_str(), backend_kind_to_json(backend_kind)?],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        Ok(())
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
        self.ensure_worktree_binding_consistency(&mapping.worktree)?;
        self.ensure_session_binding_consistency(&mapping.session)?;
        self.ensure_runtime_mapping_identity_stable(mapping)?;

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
                ORDER BY s.updated_at DESC, wi.work_item_id ASC
                LIMIT 1
                ",
                params![provider_to_str(provider), provider_ticket_id],
                Self::map_runtime_mapping_row,
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn find_runtime_mapping_by_session_id(
        &self,
        session_id: &WorkerSessionId,
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
                WHERE s.session_id = ?1
                ORDER BY s.updated_at DESC, wi.work_item_id ASC
                LIMIT 1
                ",
                params![session_id.as_str()],
                Self::map_runtime_mapping_row,
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn find_runtime_mapping_by_work_item_id(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<RuntimeMappingRecord>, CoreError> {
        self.find_runtime_mapping_by_work_item(work_item_id)
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
                ORDER BY s.updated_at DESC, wi.work_item_id ASC
                LIMIT 1
                ",
                params![
                    provider_to_str(provider),
                    provider_ticket_id,
                    running,
                    waiting_for_user,
                    blocked
                ],
                Self::map_runtime_mapping_row,
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
                ORDER BY t.identifier ASC, wi.work_item_id ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map([], Self::map_runtime_mapping_row)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    pub fn migrate_runtime_mapping_path(
        &mut self,
        work_item_id: &WorkItemId,
        next_path: &str,
    ) -> Result<(), CoreError> {
        if next_path.trim().is_empty() {
            return Err(CoreError::Persistence(
                "runtime mapping path migration target cannot be empty".to_owned(),
            ));
        }

        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let updated_worktrees = tx
            .execute(
                "
                UPDATE worktrees
                SET path = ?1
                WHERE work_item_id = ?2
                ",
                params![next_path, work_item_id.as_str()],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let updated_sessions = tx
            .execute(
                "
                UPDATE sessions
                SET workdir = ?1
                WHERE work_item_id = ?2
                ",
                params![next_path, work_item_id.as_str()],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        if updated_worktrees == 0 || updated_sessions == 0 {
            return Err(CoreError::Persistence(format!(
                "cannot migrate runtime mapping path for work_item_id '{}': missing worktree/session binding",
                work_item_id.as_str()
            )));
        }

        tx.commit()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        Ok(())
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
                ORDER BY t.identifier ASC, wi.work_item_id ASC
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

    pub fn upsert_project_repository_mapping(
        &self,
        mapping: &ProjectRepositoryMappingRecord,
    ) -> Result<(), CoreError> {
        let repository_path = mapping.repository_path.trim().to_owned();
        if repository_path.is_empty() {
            return Err(CoreError::Configuration(
                "project repository path cannot be empty".to_owned(),
            ));
        }

        self.conn
            .execute(
                "
                INSERT INTO project_repositories (provider, project_id, repository_path)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(provider, project_id) DO UPDATE SET
                    repository_path = excluded.repository_path
                ",
                params![
                    provider_to_str(&mapping.provider),
                    mapping.project_id.as_str(),
                    repository_path,
                ],
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(())
    }

    pub fn find_project_repository_mapping(
        &self,
        provider: &TicketProvider,
        project_id: &crate::identifiers::ProjectId,
    ) -> Result<Option<String>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT repository_path
                FROM project_repositories
                WHERE provider = ?1
                  AND project_id = ?2
                ",
                params![provider_to_str(provider), project_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
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

                CREATE TABLE IF NOT EXISTS session_runtime_flags (
                    session_id TEXT PRIMARY KEY,
                    is_working INTEGER NOT NULL DEFAULT 0,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY(session_id) REFERENCES sessions(session_id)
                );

                CREATE TABLE IF NOT EXISTS event_artifact_refs (
                    event_id TEXT NOT NULL,
                    artifact_id TEXT NOT NULL,
                    PRIMARY KEY (event_id, artifact_id),
                    FOREIGN KEY(event_id) REFERENCES events(event_id),
                    FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                );

                CREATE TABLE IF NOT EXISTS project_repositories (
                    provider TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    repository_path TEXT NOT NULL,
                    PRIMARY KEY (provider, project_id)
                );

                CREATE TABLE IF NOT EXISTS harness_session_bindings (
                    session_id TEXT NOT NULL,
                    backend_kind TEXT NOT NULL,
                    harness_session_id TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (session_id, backend_kind),
                    FOREIGN KEY(session_id) REFERENCES sessions(session_id)
                );

                INSERT OR IGNORE INTO session_runtime_flags (session_id, is_working, updated_at)
                SELECT session_id, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                FROM sessions;

                CREATE INDEX IF NOT EXISTS idx_events_work_item_sequence ON events(work_item_id, sequence ASC);
                CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id, sequence DESC);
                CREATE INDEX IF NOT EXISTS idx_tickets_provider_lookup ON tickets(provider, provider_ticket_id);
                CREATE INDEX IF NOT EXISTS idx_event_artifact_refs_event ON event_artifact_refs(event_id);
                CREATE INDEX IF NOT EXISTS idx_worktrees_work_item_lookup ON worktrees(work_item_id);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_worktrees_path_unique ON worktrees(path);
                CREATE INDEX IF NOT EXISTS idx_sessions_work_item_lookup ON sessions(work_item_id);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_workdir_unique ON sessions(workdir);
                CREATE INDEX IF NOT EXISTS idx_sessions_status_lookup ON sessions(status);
                CREATE INDEX IF NOT EXISTS idx_sessions_status_updated_lookup ON sessions(status, updated_at);
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
                3 => tx
                    .execute_batch(
                        "
                        CREATE UNIQUE INDEX IF NOT EXISTS idx_worktrees_path_unique ON worktrees(path);
                        CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_workdir_unique ON sessions(workdir);
                        ",
                    )
                    .map_err(|err| CoreError::Persistence(err.to_string())),
            4 => tx
                .execute_batch(
                    "
                    CREATE TABLE IF NOT EXISTS project_repositories (
                        provider TEXT NOT NULL,
                        project_id TEXT NOT NULL,
                        repository_path TEXT NOT NULL,
                        PRIMARY KEY (provider, project_id)
                    );
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            5 => tx
                .execute_batch(
                    "
                    CREATE TABLE IF NOT EXISTS harness_session_bindings (
                        session_id TEXT NOT NULL,
                        backend_kind TEXT NOT NULL,
                        harness_session_id TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        PRIMARY KEY (session_id, backend_kind),
                        FOREIGN KEY(session_id) REFERENCES sessions(session_id)
                    );
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            6 => tx
                .execute_batch(
                    "
                    CREATE TABLE IF NOT EXISTS session_workflow_stages (
                        session_id TEXT PRIMARY KEY,
                        workflow_stage TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        FOREIGN KEY(session_id) REFERENCES sessions(session_id)
                    );
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            7 => tx
                .execute_batch(
                    "
                    DROP TABLE IF EXISTS session_workflow_stages;
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            8 => tx
                .execute_batch(
                    "
                    CREATE TABLE IF NOT EXISTS session_runtime_flags (
                        session_id TEXT PRIMARY KEY,
                        is_working INTEGER NOT NULL DEFAULT 0,
                        updated_at TEXT NOT NULL,
                        FOREIGN KEY(session_id) REFERENCES sessions(session_id)
                    );

                    INSERT OR IGNORE INTO session_runtime_flags (session_id, is_working, updated_at)
                    SELECT session_id, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                    FROM sessions;
                    ",
                )
                .map_err(|err| CoreError::Persistence(err.to_string())),
            9 => tx
                .execute_batch(
                    "
                    CREATE INDEX IF NOT EXISTS idx_sessions_status_updated_lookup ON sessions(status, updated_at);
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

    fn ensure_worktree_binding_consistency(
        &self,
        worktree: &WorktreeRecord,
    ) -> Result<(), CoreError> {
        if let Some(existing) = self.find_worktree_by_work_item(&worktree.work_item_id)? {
            if existing.worktree_id != worktree.worktree_id
                || existing.path != worktree.path
                || existing.branch != worktree.branch
                || existing.base_branch != worktree.base_branch
            {
                return Err(CoreError::Persistence(format!(
                    "work_item_id '{}' is already bound to worktree_id '{}' at path '{}'; refusing reassignment to worktree_id '{}' at path '{}'",
                    worktree.work_item_id.as_str(),
                    existing.worktree_id.as_str(),
                    existing.path,
                    worktree.worktree_id.as_str(),
                    worktree.path
                )));
            }
        }

        if let Some(existing_work_item) =
            self.find_work_item_id_by_worktree_id(&worktree.worktree_id)?
        {
            if existing_work_item != worktree.work_item_id {
                return Err(CoreError::Persistence(format!(
                    "worktree_id '{}' is already mapped to work_item_id '{}'",
                    worktree.worktree_id.as_str(),
                    existing_work_item.as_str()
                )));
            }
        }

        if let Some(existing_work_item) = self.find_work_item_id_by_worktree_path(&worktree.path)? {
            if existing_work_item != worktree.work_item_id {
                return Err(CoreError::Persistence(format!(
                    "worktree path '{}' is already mapped to work_item_id '{}'",
                    worktree.path,
                    existing_work_item.as_str()
                )));
            }
        }

        Ok(())
    }

    fn ensure_session_binding_consistency(&self, session: &SessionRecord) -> Result<(), CoreError> {
        if let Some(existing) = self.find_session_by_work_item(&session.work_item_id)? {
            if existing.session_id != session.session_id
                || existing.backend_kind != session.backend_kind
                || existing.workdir != session.workdir
            {
                return Err(CoreError::Persistence(format!(
                    "work_item_id '{}' is already bound to session_id '{}' at workdir '{}'; refusing reassignment to session_id '{}' at workdir '{}'",
                    session.work_item_id.as_str(),
                    existing.session_id.as_str(),
                    existing.workdir,
                    session.session_id.as_str(),
                    session.workdir
                )));
            }
        }

        if let Some(existing_work_item) =
            self.find_work_item_id_by_session_workdir(&session.workdir)?
        {
            if existing_work_item != session.work_item_id {
                return Err(CoreError::Persistence(format!(
                    "session workdir '{}' is already mapped to work_item_id '{}'",
                    session.workdir,
                    existing_work_item.as_str()
                )));
            }
        }

        if let Some(existing_work_item) =
            self.find_work_item_id_by_session_id(&session.session_id)?
        {
            if existing_work_item != session.work_item_id {
                return Err(CoreError::Persistence(format!(
                    "session_id '{}' is already mapped to work_item_id '{}'",
                    session.session_id.as_str(),
                    existing_work_item.as_str()
                )));
            }
        }

        Ok(())
    }

    fn ensure_runtime_mapping_identity_stable(
        &self,
        mapping: &RuntimeMappingRecord,
    ) -> Result<(), CoreError> {
        if let Some(existing) = self.find_runtime_mapping_by_work_item(&mapping.work_item_id)? {
            if existing.ticket.ticket_id != mapping.ticket.ticket_id {
                return Err(CoreError::Persistence(format!(
                    "work_item_id '{}' is already mapped to ticket_id '{}'",
                    mapping.work_item_id.as_str(),
                    existing.ticket.ticket_id.as_str()
                )));
            }

            if existing.worktree.worktree_id != mapping.worktree.worktree_id
                || existing.worktree.path != mapping.worktree.path
                || existing.worktree.branch != mapping.worktree.branch
                || existing.worktree.base_branch != mapping.worktree.base_branch
                || existing.session.session_id != mapping.session.session_id
                || existing.session.backend_kind != mapping.session.backend_kind
                || existing.session.workdir != mapping.session.workdir
            {
                return Err(CoreError::Persistence(format!(
                    "runtime mapping for work_item_id '{}' is already bound to worktree '{}' and session '{}'; refusing reassignment to worktree '{}' and session '{}' to preserve deterministic resume",
                    mapping.work_item_id.as_str(),
                    existing.worktree.path,
                    existing.session.session_id.as_str(),
                    mapping.worktree.path,
                    mapping.session.session_id.as_str()
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

    fn find_work_item_id_by_worktree_path(
        &self,
        worktree_path: &str,
    ) -> Result<Option<WorkItemId>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT work_item_id
                FROM worktrees
                WHERE path = ?1
                ",
                params![worktree_path],
                |row| row.get::<_, String>(0).map(WorkItemId::from),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn find_work_item_id_by_worktree_id(
        &self,
        worktree_id: &crate::identifiers::WorktreeId,
    ) -> Result<Option<WorkItemId>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT work_item_id
                FROM worktrees
                WHERE worktree_id = ?1
                ",
                params![worktree_id.as_str()],
                |row| row.get::<_, String>(0).map(WorkItemId::from),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn find_work_item_id_by_session_workdir(
        &self,
        workdir: &str,
    ) -> Result<Option<WorkItemId>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT work_item_id
                FROM sessions
                WHERE workdir = ?1
                ",
                params![workdir],
                |row| row.get::<_, String>(0).map(WorkItemId::from),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn find_work_item_id_by_session_id(
        &self,
        session_id: &WorkerSessionId,
    ) -> Result<Option<WorkItemId>, CoreError> {
        self.conn
            .query_row(
                "
                SELECT work_item_id
                FROM sessions
                WHERE session_id = ?1
                ",
                params![session_id.as_str()],
                |row| row.get::<_, String>(0).map(WorkItemId::from),
            )
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn find_runtime_mapping_by_work_item(
        &self,
        work_item_id: &WorkItemId,
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
                WHERE wi.work_item_id = ?1
                LIMIT 1
                ",
                params![work_item_id.as_str()],
                Self::map_runtime_mapping_row,
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

        tx.execute(
            "
            INSERT INTO session_runtime_flags (session_id, is_working, updated_at)
            VALUES (?1, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            ON CONFLICT(session_id) DO NOTHING
            ",
            params![session.session_id.as_str()],
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

    pub fn read_artifact_refs_for_event(
        &self,
        event_id: &str,
    ) -> Result<Vec<ArtifactId>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT artifact_id
                FROM event_artifact_refs
                WHERE event_id = ?1
                ORDER BY rowid ASC
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

    fn map_rfc3339_unix_seconds(value: &str) -> Option<u64> {
        let timestamp = OffsetDateTime::parse(value, &Rfc3339).ok()?;
        let unix = timestamp.unix_timestamp();
        (unix >= 0).then_some(unix as u64)
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

fn parse_unix_seconds(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rfc3339) = SqliteEventStore::map_rfc3339_unix_seconds(trimmed) {
        return Some(rfc3339);
    }

    let without_z = trimmed.strip_suffix('Z').unwrap_or(trimmed);
    let whole_seconds = without_z
        .split_once('.')
        .map(|(left, _)| left)
        .unwrap_or(without_z);
    whole_seconds.parse::<u64>().ok()
}
