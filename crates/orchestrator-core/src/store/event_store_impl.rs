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

