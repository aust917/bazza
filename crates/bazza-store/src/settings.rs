use crate::{Result, Store};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Serialize};

/// Remove disposable observations of this meeting, across voice policies. A
/// policy is one nonempty namespace component; compare the complete remaining
/// ID, rather than using wildcard patterns or a suffix that can match another
/// meeting. Keep this in the caller's transaction with its transcript change.
pub(crate) fn clear_meeting_voice_queries_on(
    tx: &Transaction<'_>,
    meeting_id: &str,
) -> rusqlite::Result<()> {
    const PREFIX: &str = "speaker_embeddings:";
    tx.execute(
        "WITH scoped_keys AS (
            SELECT key,substr(key,length(?1)+1) AS rest FROM settings
            WHERE substr(key,1,length(?1))=?1
         )
         DELETE FROM settings WHERE key=?2 OR key IN (
            SELECT key FROM scoped_keys WHERE instr(rest,':')>1
            AND substr(rest,instr(rest,':')+1)=?3
         )",
        params![PREFIX, format!("{PREFIX}{meeting_id}"), meeting_id],
    )?;
    Ok(())
}

impl Store {
    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.conn.lock();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key=?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match raw {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, v: &T) -> Result<()> {
        let json = serde_json::to_string(v)?;
        self.conn.lock().execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, json],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_json_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        s.set_setting("video_enabled", &true).unwrap();
        assert_eq!(s.get_setting::<bool>("video_enabled").unwrap(), Some(true));
        assert_eq!(s.get_setting::<bool>("missing").unwrap(), None);
        s.set_setting("video_enabled", &false).unwrap();
        assert_eq!(s.get_setting::<bool>("video_enabled").unwrap(), Some(false));
    }

    #[test]
    fn voice_query_cleanup_matches_the_full_id_without_wildcards() {
        let store = Store::open_in_memory().unwrap();
        let id = "part_%[a]";
        let removed = [
            format!("speaker_embeddings:{id}"),
            format!("speaker_embeddings:policy-v1:{id}"),
            format!("speaker_embeddings:future_policy%*?:{id}"),
        ];
        let retained = [
            format!("speaker_embeddings:policy-v1:prefix{id}"),
            format!("speaker_embeddings:policy-v1:{id}-later"),
            "speaker_embeddings:policy-v1:part_AB[a]".into(),
            format!("speaker_embeddings:policy:extra:{id}"),
            format!("speaker_embeddings::{id}"),
            format!("speaker_embeddings_other:policy-v1:{id}"),
            format!("Speaker_embeddings:policy-v1:{id}"),
            format!("highlights:{id}"),
            "settings".into(),
            "private:provider_key".into(),
        ];
        for key in removed.iter().chain(&retained) {
            store
                .set_setting(key, &"unchanged synthetic value")
                .unwrap();
        }
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            clear_meeting_voice_queries_on(&tx, id).unwrap();
            tx.commit().unwrap();
        }
        for key in &removed {
            assert!(store.get_setting::<String>(key).unwrap().is_none(), "{key}");
        }
        for key in &retained {
            assert_eq!(
                store.get_setting::<String>(key).unwrap().as_deref(),
                Some("unchanged synthetic value"),
                "{key}"
            );
        }
    }

    #[test]
    fn voice_query_cleanup_rolls_back_with_its_transaction() {
        let store = Store::open_in_memory().unwrap();
        for key in [
            "speaker_embeddings:meeting",
            "speaker_embeddings:policy:meeting",
        ] {
            store.set_setting(key, &vec![0.25_f32, 0.75]).unwrap();
        }
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            clear_meeting_voice_queries_on(&tx, "meeting").unwrap();
            assert_eq!(
                tx.query_row("SELECT COUNT(*) FROM settings", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        for key in [
            "speaker_embeddings:meeting",
            "speaker_embeddings:policy:meeting",
        ] {
            assert_eq!(
                store.get_setting::<Vec<f32>>(key).unwrap(),
                Some(vec![0.25, 0.75])
            );
        }
    }
}
