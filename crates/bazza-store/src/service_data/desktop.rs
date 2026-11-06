//! Narrow desktop repository projections. No generic settings/SQL/file access,
//! capture defaults, cloud consent or secrets are exposed through this boundary.
use super::*;
use crate::{MeetingSpeaker, SpeakerStat, Store};
use chrono::{DateTime, Utc};
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_PAGE: u32 = 65_536;
const SETTINGS_BYTES: usize = 1_048_576;
const MAX_TIME: i64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopLocale {
    Ru,
    En,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopPreferences {
    pub revision: String,
    pub ui_locale: DesktopLocale,
    pub close_to_tray: bool,
    pub hotkey: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopPreferencesPatch {
    pub ui_locale: Option<DesktopLocale>,
    pub close_to_tray: Option<bool>,
    pub hotkey: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopBootstrap {
    pub preferences: DesktopPreferences,
    pub change_sequence: i64,
    pub cursor_scope: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopMedia {
    pub audio: bool,
    pub video: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopMeeting {
    pub id: String,
    pub title: String,
    pub app: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: i64,
    pub has_video: bool,
    pub status: String,
    pub folder_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub error: Option<String>,
    pub error_truncated: bool,
    pub title_auto: bool,
    pub participants: Vec<String>,
    pub calendar_uid: Option<String>,
    pub content_kind: String,
    pub metadata_revision: String,
    pub lifecycle_revision: String,
    pub transcript_revision: String,
    pub transcript_state: String,
    pub summary_revision: String,
    pub summary_state: String,
    pub media: DesktopMedia,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopFolderFilter {
    #[default]
    All,
    Unfiled,
    Subtree {
        folder_id: String,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeetingFilter {
    #[serde(default)]
    pub folder: DesktopFolderFilter,
    pub tag_id: Option<String>,
    pub content_kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopCursor {
    pub library_scope: String,
    pub snapshot_sequence: i64,
    pub query_hash: String,
    pub offset: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeetingRequest {
    #[serde(default)]
    pub filter: DesktopMeetingFilter,
    pub cursor: Option<DesktopCursor>,
    pub limit: u16,
    pub max_bytes: u32,
}
impl Default for DesktopMeetingRequest {
    fn default() -> Self {
        Self {
            filter: DesktopMeetingFilter::default(),
            cursor: None,
            limit: 20,
            max_bytes: MAX_PAGE,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopMeetingPage {
    pub items: Vec<DesktopMeeting>,
    pub next: Option<DesktopCursor>,
    pub snapshot_sequence: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopChangeCursor {
    pub library_scope: String,
    pub after: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopChangesRequest {
    pub cursor: Option<DesktopChangeCursor>,
    pub limit: u16,
    pub max_bytes: u32,
}
impl Default for DesktopChangesRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: 100,
            max_bytes: MAX_PAGE,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopChangePage {
    pub items: Vec<Change>,
    pub next: DesktopChangeCursor,
    pub has_more: bool,
    pub current_sequence: i64,
}

fn page_limits(limit: u16, max_bytes: u32) -> DataResult<()> {
    if !(1..=100).contains(&limit) || !(1024..=MAX_PAGE).contains(&max_bytes) {
        return Err(DataError::Invalid("desktop page budget"));
    }
    Ok(())
}

fn fits<T: Serialize>(value: &T, max: usize) -> bool {
    struct Budget {
        used: usize,
        max: usize,
    }
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|n| *n <= self.max)
                .ok_or_else(|| std::io::Error::other("desktop response budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget { used: 0, max }, value).is_ok()
}

fn read_settings(conn: &Connection) -> DataResult<serde_json::Map<String, serde_json::Value>> {
    let raw:Option<Option<String>>=conn.query_row(
        "SELECT CASE WHEN length(CAST(value AS BLOB))<=1048576 THEN value END FROM settings WHERE key='settings'",
        [],|r|r.get(0)).optional()?;
    match raw {
        None => Ok(serde_json::Map::new()),
        Some(Some(raw)) => match serde_json::from_str(&raw) {
            Ok(serde_json::Value::Object(value)) => Ok(value),
            _ => Err(DataError::Invalid("malformed desktop settings")),
        },
        Some(None) => Err(DataError::Invalid("desktop settings budget")),
    }
}

fn preference_revision(conn: &Connection) -> DataResult<String> {
    let revision: String = conn.query_row(
        "SELECT revision FROM service_desktop_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    bounded_text(&revision, 1, 128)?;
    if !revision
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
    {
        return Err(DataError::Invalid("desktop revision"));
    }
    Ok(revision)
}

fn preference_fields(
    settings: &serde_json::Map<String, serde_json::Value>,
) -> DataResult<(bool, String)> {
    let close_to_tray = match settings.get("close_to_tray") {
        None => true,
        Some(value) => value
            .as_bool()
            .ok_or(DataError::Invalid("malformed close-to-tray preference"))?,
    };
    let hotkey: String = match settings.get("hotkey") {
        None => "CmdOrCtrl+Shift+R".into(),
        Some(value) => value
            .as_str()
            .ok_or(DataError::Invalid("malformed hotkey preference"))?
            .into(),
    };
    bounded_text(&hotkey, 1, 256)?;
    Ok((close_to_tray, hotkey))
}

fn preferences_on(conn: &Connection) -> DataResult<DesktopPreferences> {
    let settings = read_settings(conn)?;
    let (close_to_tray, hotkey) = preference_fields(&settings)?;
    let raw:Option<Option<String>>=conn.query_row("SELECT CASE WHEN length(CAST(value AS BLOB))<=64 THEN value END FROM settings WHERE key='ui_locale'",[],|r|r.get(0)).optional()?;
    // Preserve the GUI's independent locale policy: absent -> Russian;
    // malformed or unsupported saved locale -> English, without rewriting it.
    let ui_locale = match raw {
        None => DesktopLocale::Ru,
        Some(Some(raw)) => serde_json::from_str(&raw).unwrap_or(DesktopLocale::En),
        Some(None) => DesktopLocale::En,
    };
    Ok(DesktopPreferences {
        revision: preference_revision(conn)?,
        ui_locale,
        close_to_tray,
        hotkey,
    })
}

pub(super) fn snapshot_on(conn: &Connection) -> DataResult<(String, i64)> {
    let scope: String = conn.query_row(
        "SELECT cursor_scope FROM service_search_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if scope.len() != 32 || !scope.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DataError::Invalid("desktop cursor scope"));
    }
    // AUTOINCREMENT retains the high-water mark even after old events are
    // pruned. Bootstrap must never move a reconnecting client backwards.
    let sequence:i64=conn.query_row("SELECT MAX(COALESCE((SELECT MAX(sequence) FROM service_changes),0),COALESCE((SELECT seq FROM sqlite_sequence WHERE name='service_changes'),0))",[],|r|r.get(0))?;
    if !(0..=MAX_TIME).contains(&sequence) {
        return Err(DataError::Invalid("desktop change sequence"));
    }
    Ok((scope, sequence))
}

pub(super) const MEETING_COLUMNS: &str = r#"
CASE WHEN length(CAST(m.id AS BLOB)) BETWEEN 1 AND 128 THEN m.id END AS id,
CASE WHEN length(CAST(m.title AS BLOB)) BETWEEN 1 AND 512 THEN m.title END AS title,
CASE WHEN length(CAST(m.app AS BLOB))<=512 THEN m.app END AS app,
CASE WHEN length(CAST(m.started_at AS BLOB))<=64 THEN m.started_at END AS started_at,
CASE WHEN length(CAST(m.ended_at AS BLOB))<=64 THEN m.ended_at END AS ended_at,
m.duration_ms,m.has_video,
CASE WHEN m.status IN ('recording','processing','ready','failed') THEN m.status ELSE 'failed' END AS status,
CASE WHEN length(CAST(m.folder_id AS BLOB))<=128 THEN m.folder_id END AS folder_id,
CASE WHEN length(CAST(m.created_at AS BLOB))<=64 THEN m.created_at END AS created_at,
substr(m.error,1,2048) AS error,length(CAST(m.error AS BLOB))>2048 AS error_truncated,
m.title_auto,
CASE WHEN length(CAST(m.participants_json AS BLOB))<=32768 THEN m.participants_json END AS participants_json,
CASE WHEN length(CAST(m.calendar_uid AS BLOB))<=512 THEN m.calendar_uid END AS calendar_uid,
s.content_kind,
CASE WHEN length(CAST(s.metadata_revision AS BLOB)) BETWEEN 1 AND 128 THEN s.metadata_revision END AS metadata_revision,
CASE WHEN length(CAST(s.lifecycle_revision AS BLOB)) BETWEEN 1 AND 128 THEN s.lifecycle_revision END AS lifecycle_revision,
CASE WHEN length(CAST(t.revision AS BLOB)) BETWEEN 1 AND 128 THEN t.revision END AS transcript_revision,
t.document_state AS transcript_state,
CASE WHEN length(CAST(u.revision AS BLOB)) BETWEEN 1 AND 128 THEN u.revision END AS summary_revision,
u.document_state AS summary_state,
(m.app IS NOT NULL AND length(CAST(m.app AS BLOB))>512)
 OR (m.folder_id IS NOT NULL AND length(CAST(m.folder_id AS BLOB))>128)
 OR (m.calendar_uid IS NOT NULL AND length(CAST(m.calendar_uid AS BLOB))>512)
 OR (m.ended_at IS NOT NULL AND length(CAST(m.ended_at AS BLOB))>64) AS oversized_optional
"#;
pub(super) const MEETING_FROM:&str="FROM meetings m JOIN service_meeting_state s ON s.meeting_id=m.id JOIN service_transcript_state t ON t.meeting_id=m.id JOIN service_summary_state u ON u.meeting_id=m.id";

pub(super) fn row_error(error: rusqlite::Error) -> DataError {
    if projection_error(&error) {
        return DataError::Invalid("legacy data cannot be represented by desktop projection");
    }
    DataError::Sqlite(error)
}

fn projection_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::InvalidQuery
            | rusqlite::Error::InvalidColumnType(_, _, _)
            | rusqlite::Error::FromSqlConversionFailure(_, _, _)
            | rusqlite::Error::IntegralValueOutOfRange(_, _)
            | rusqlite::Error::Utf8Error(_, _)
    )
}

pub(super) fn meeting_row(r: &Row<'_>) -> rusqlite::Result<DesktopMeeting> {
    if r.get::<_, bool>("oversized_optional")? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let encoded: String = r.get("participants_json")?;
    let participants: Vec<String> =
        serde_json::from_str(&encoded).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if participants.len() > 64
        || participants
            .iter()
            .any(|p| p.is_empty() || p.len() > 256 || p.contains('\0'))
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut error: Option<String> = r.get("error")?;
    if let Some(text) = &mut error {
        let mut end = text.len().min(2048);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    let result = DesktopMeeting {
        id: r.get("id")?,
        title: r.get("title")?,
        app: r.get("app")?,
        started_at: r.get("started_at")?,
        ended_at: r.get("ended_at")?,
        duration_ms: r.get("duration_ms")?,
        has_video: r.get("has_video")?,
        status: r.get("status")?,
        folder_id: r.get("folder_id")?,
        created_at: r.get("created_at")?,
        error,
        error_truncated: r
            .get::<_, Option<bool>>("error_truncated")?
            .unwrap_or(false),
        title_auto: r.get("title_auto")?,
        participants,
        calendar_uid: r.get("calendar_uid")?,
        content_kind: r.get("content_kind")?,
        metadata_revision: r.get("metadata_revision")?,
        lifecycle_revision: r.get("lifecycle_revision")?,
        transcript_revision: r.get("transcript_revision")?,
        transcript_state: r.get("transcript_state")?,
        summary_revision: r.get("summary_revision")?,
        summary_state: r.get("summary_state")?,
        media: DesktopMedia::default(),
    };
    if !(0..=MAX_TIME).contains(&result.duration_ms)
        || safe_id(&result.id).is_err()
        || result
            .folder_id
            .as_ref()
            .is_some_and(|id| safe_id(id).is_err())
        || result.title.contains('\0')
        || result.app.as_ref().is_some_and(|s| s.contains('\0'))
        || result
            .calendar_uid
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.contains('\0'))
        || result.error.as_ref().is_some_and(|s| s.contains('\0'))
        || [
            &result.metadata_revision,
            &result.lifecycle_revision,
            &result.transcript_revision,
            &result.summary_revision,
        ]
        .iter()
        .any(|s| {
            !s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        })
        || !["recorded", "text"].contains(&result.content_kind.as_str())
        || !["absent", "present", "deleted"].contains(&result.transcript_state.as_str())
        || !["absent", "present", "deleted"].contains(&result.summary_state.as_str())
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(result)
}

fn regular(path: &std::path::Path, dir: bool) -> bool {
    path.symlink_metadata().is_ok_and(|metadata| {
        let linked = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let linked = {
            use std::os::windows::fs::MetadataExt;
            linked || metadata.file_attributes() & 0x400 != 0
        };
        !linked
            && if dir {
                metadata.is_dir()
            } else {
                metadata.is_file()
            }
    })
}

pub(super) fn media(store: &Store, item: &mut DesktopMeeting) {
    if item.content_kind == "text" {
        return;
    }
    let parent = store.root().join("recordings");
    let directory = parent.join(&item.id);
    if !regular(&parent, true) || !regular(&directory, true) {
        return;
    }
    item.media.audio = regular(&directory.join("mix.wav"), false);
    item.media.video = item.has_video && regular(&directory.join("screen.mp4"), false);
}

#[derive(Debug, Serialize)]
pub struct DesktopMeetingDecorations {
    meeting_id: String,
    highlights: Vec<u64>,
    speakers: Vec<MeetingSpeaker>,
    speaker_stats: Vec<SpeakerStat>,
}

impl Store {
    pub fn service_desktop_meeting_decorations(
        &self,
        id: &str,
    ) -> DataResult<DesktopMeetingDecorations> {
        const MAX_HIGHLIGHT_JSON: i64 = 8_192;
        const MAX_SPEAKERS: usize = 128;
        safe_id(id)?;
        let conn = self.conn.lock();
        visible_meeting(&conn, id)?;
        let highlights: Option<Option<String>> = conn.query_row(
            "SELECT CASE WHEN length(CAST(value AS BLOB)) <= ?2 THEN value END FROM settings WHERE key=?1",
            params![format!("highlights:{id}"), MAX_HIGHLIGHT_JSON],
            |row| row.get(0),
        ).optional()?;
        let highlights = match highlights {
            Some(Some(raw)) => serde_json::from_str(&raw)?,
            Some(None) => return Err(DataError::Invalid("desktop highlights budget")),
            None => Vec::new(),
        };
        let overlong: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_speakers ms LEFT JOIN speakers s ON s.id=ms.suggested_id
                WHERE ms.meeting_id=?1 AND MAX(length(CAST(ms.label AS BLOB)),
                    COALESCE(length(CAST(ms.speaker_id AS BLOB)),0),
                    COALESCE(length(CAST(ms.suggested_id AS BLOB)),0),
                    COALESCE(length(CAST(s.name AS BLOB)),0))>512)
             OR EXISTS(SELECT 1 FROM segments WHERE meeting_id=?1 AND length(CAST(speaker AS BLOB))>512)",
            [id], |row| row.get(0),
        )?;
        if overlong {
            return Err(DataError::Invalid("desktop speaker budget"));
        }
        let mut speakers = conn.prepare(
            "SELECT ms.label, ms.speaker_id, ms.suggested_id, s.name, ms.suggested_score
             FROM meeting_speakers ms LEFT JOIN speakers s ON s.id = ms.suggested_id
             WHERE ms.meeting_id = ?1 ORDER BY ms.label LIMIT ?2",
        )?;
        let speakers = speakers
            .query_map(params![id, MAX_SPEAKERS as i64 + 1], |row| {
                Ok(MeetingSpeaker {
                    meeting_id: id.to_owned(),
                    label: row.get(0)?,
                    speaker_id: row.get(1)?,
                    suggested_id: row.get(2)?,
                    suggested_name: row.get(3)?,
                    suggested_score: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stats = conn.prepare(
            "SELECT speaker, COUNT(*), SUM(MAX(end_ms-start_ms,0)), MIN(start_ms) FROM segments
             WHERE meeting_id=?1 AND speaker IS NOT NULL AND TRIM(speaker)<>''
             GROUP BY speaker ORDER BY MIN(start_ms) LIMIT ?2",
        )?;
        let speaker_stats = stats
            .query_map(params![id, MAX_SPEAKERS as i64 + 1], |row| {
                Ok(SpeakerStat {
                    label: row.get(0)?,
                    segments: row.get(1)?,
                    ms: row.get(2)?,
                    first_ms: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if speakers.len() > MAX_SPEAKERS || speaker_stats.len() > MAX_SPEAKERS {
            return Err(DataError::Invalid("desktop speaker budget"));
        }
        Ok(DesktopMeetingDecorations {
            meeting_id: id.to_owned(),
            highlights,
            speakers,
            speaker_stats,
        })
    }
}

impl Store {
    pub fn service_desktop_bootstrap(&self) -> DataResult<DesktopBootstrap> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (cursor_scope, change_sequence) = snapshot_on(&tx)?;
        let result = DesktopBootstrap {
            preferences: preferences_on(&tx)?,
            change_sequence,
            cursor_scope,
        };
        if !fits(&result, MAX_PAGE as usize) {
            return Err(DataError::Invalid("desktop bootstrap budget"));
        }
        Ok(result)
    }
    pub fn service_desktop_preferences(&self) -> DataResult<DesktopPreferences> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        preferences_on(&tx)
    }
    pub fn service_update_desktop_preferences(
        &self,
        ctx: &MutationContext,
        expected: &str,
        patch: &DesktopPreferencesPatch,
    ) -> DataResult<MutationOutcome> {
        if patch.ui_locale.is_none() && patch.close_to_tray.is_none() && patch.hotkey.is_none() {
            return Err(DataError::Invalid("empty desktop preferences patch"));
        }
        if let Some(hotkey) = &patch.hotkey {
            bounded_text(hotkey, 1, 256)?;
        }
        consistency::mutate(
            self,
            ctx,
            "update_desktop_preferences",
            &(expected, patch),
            |tx| {
                check_revision(&preference_revision(tx)?, expected)?;
                if patch.close_to_tray.is_some() || patch.hotkey.is_some() {
                    let mut settings = read_settings(tx)?;
                    if let Some(close) = patch.close_to_tray {
                        settings.insert("close_to_tray".into(), close.into());
                    }
                    if let Some(hotkey) = &patch.hotkey {
                        settings.insert("hotkey".into(), hotkey.clone().into());
                    }
                    preference_fields(&settings)?;
                    if !fits(&settings, SETTINGS_BYTES) {
                        return Err(DataError::Invalid("desktop settings budget"));
                    }
                    tx.execute("INSERT INTO settings(key,value) VALUES('settings',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&settings)?])?;
                }
                if let Some(locale) = patch.ui_locale {
                    tx.execute("INSERT INTO settings(key,value) VALUES('ui_locale',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&locale)?])?;
                }
                let rev = revision();
                tx.execute(
                    "UPDATE service_desktop_state SET revision=?1 WHERE singleton=1",
                    [&rev],
                )?;
                consistency::outcome(tx, "desktop_preferences", "desktop_preferences", &rev, ctx)
            },
        )
    }
    pub fn service_desktop_rename_meeting(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
        title: &str,
    ) -> DataResult<MutationOutcome> {
        self.service_update_meeting(
            ctx,
            id,
            expected,
            &MeetingUpdate {
                title: Some(title.into()),
                ..Default::default()
            },
        )
    }
    pub fn service_desktop_meeting(&self, id: &str) -> DataResult<Option<DesktopMeeting>> {
        safe_id(id)?;
        let mut item = {
            let conn = self.conn.lock();
            conn.query_row(
                &format!(
                    "SELECT {MEETING_COLUMNS} {MEETING_FROM} WHERE m.id=?1 AND s.tombstoned=0"
                ),
                [id],
                meeting_row,
            )
            .optional()
            .map_err(row_error)?
        };
        if let Some(item) = &mut item {
            media(self, item);
            if !fits(item, MAX_PAGE as usize) {
                return Err(DataError::Invalid("desktop meeting budget"));
            }
        }
        Ok(item)
    }
    pub fn service_desktop_meetings(
        &self,
        request: &DesktopMeetingRequest,
    ) -> DataResult<DesktopMeetingPage> {
        page_limits(request.limit, request.max_bytes)?;
        let (folder, mode) = match &request.filter.folder {
            DesktopFolderFilter::All => (None, 0),
            DesktopFolderFilter::Unfiled => (None, 1),
            DesktopFolderFilter::Subtree { folder_id } => {
                safe_id(folder_id)?;
                (Some(folder_id.as_str()), 2)
            }
        };
        if let Some(tag) = &request.filter.tag_id {
            safe_id(tag)?;
        }
        if request
            .filter
            .content_kind
            .as_ref()
            .is_some_and(|kind| !["recorded", "text"].contains(&kind.as_str()))
        {
            return Err(DataError::Invalid("desktop content kind filter"));
        }
        let hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                &request.filter,
                request.limit,
                request.max_bytes
            ))?)
        );
        let offset = request.cursor.as_ref().map_or(0, |cursor| cursor.offset);
        if offset > u32::MAX as u64 {
            return Err(DataError::Invalid("desktop cursor offset"));
        }
        let mut page = {
            let mut conn = self.conn.lock();
            let tx = conn.transaction()?;
            let (scope, sequence) = snapshot_on(&tx)?;
            if request.cursor.as_ref().is_some_and(|cursor| {
                cursor.library_scope != scope
                    || cursor.snapshot_sequence != sequence
                    || cursor.query_hash != hash
            }) {
                return Err(DataError::StaleCursor);
            }
            let next_at = |count: usize| DesktopCursor {
                library_scope: scope.clone(),
                snapshot_sequence: sequence,
                query_hash: hash.clone(),
                offset: offset + count as u64,
            };
            let sql=format!("WITH RECURSIVE selected_folders(id) AS (SELECT ?1 WHERE ?1 IS NOT NULL UNION SELECT f.id FROM folders f JOIN selected_folders p ON f.parent_id=p.id) SELECT {MEETING_COLUMNS} {MEETING_FROM} WHERE s.tombstoned=0 AND (?2 IS NULL OR EXISTS(SELECT 1 FROM meeting_tags mt WHERE mt.meeting_id=m.id AND mt.tag_id=?2)) AND (?3 IS NULL OR s.content_kind=?3) AND (?4=0 OR (?4=1 AND m.folder_id IS NULL) OR (?4=2 AND m.folder_id IN (SELECT id FROM selected_folders))) ORDER BY m.started_at DESC,m.id ASC LIMIT ?5 OFFSET ?6");
            let mut statement = tx.prepare(&sql)?;
            let mut rows = statement.query(params![
                folder,
                request.filter.tag_id,
                request.filter.content_kind,
                mode,
                i64::from(request.limit) + 1,
                offset as i64
            ])?;
            let mut page = DesktopMeetingPage {
                items: Vec::new(),
                next: None,
                snapshot_sequence: sequence,
            };
            loop {
                if page.items.len() == request.limit as usize {
                    // A pure lookahead establishes continuation without parsing
                    // an oversized/malformed row outside this requested page.
                    if rows.next()?.is_none() {
                        page.next = None;
                    }
                    break;
                }
                let Some(row) = rows.next()? else {
                    page.next = None;
                    break;
                };
                let item = match meeting_row(row) {
                    Ok(item) => item,
                    Err(error) if !page.items.is_empty() && projection_error(&error) => {
                        // Return the valid prefix. Its continuation points to
                        // this unconsumed logical row, never silently past it.
                        page.next = Some(next_at(page.items.len()));
                        break;
                    }
                    Err(error) => return Err(row_error(error)),
                };
                page.items.push(item);
                // false serializes longer than true; media remains false until
                // after the DB snapshot/mutex is released. Reserve a cursor
                // while continuation is unknown so this bound stays conservative.
                page.next = Some(next_at(page.items.len()));
                if !fits(&page, request.max_bytes as usize) {
                    // A terminal page may fit once an unnecessary cursor is
                    // removed; the lookahead itself is deliberately not mapped.
                    if rows.next()?.is_none() {
                        page.next = None;
                        if fits(&page, request.max_bytes as usize) {
                            break;
                        }
                    }
                    page.items.pop();
                    if page.items.is_empty() {
                        return Err(DataError::Invalid(
                            "one desktop meeting exceeds page budget",
                        ));
                    }
                    page.next = Some(next_at(page.items.len()));
                    break;
                }
            }
            page
        };
        for item in &mut page.items {
            media(self, item);
        }
        Ok(page)
    }
    pub fn service_desktop_changes(
        &self,
        request: &DesktopChangesRequest,
    ) -> DataResult<DesktopChangePage> {
        page_limits(request.limit, request.max_bytes)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (scope, current_sequence) = snapshot_on(&tx)?;
        let after = request.cursor.as_ref().map_or(0, |cursor| cursor.after);
        if after < 0
            || after > current_sequence
            || request
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.library_scope != scope)
        {
            return Err(DataError::StaleCursor);
        }
        let mut statement=tx.prepare("SELECT sequence,CASE WHEN length(CAST(entity_kind AS BLOB))<=64 THEN entity_kind END,CASE WHEN length(CAST(entity_id AS BLOB))<=CASE WHEN entity_kind='template' THEN 512 ELSE 128 END THEN entity_id END,CASE WHEN length(CAST(revision AS BLOB))<=128 THEN revision END,CASE WHEN length(CAST(actor AS BLOB))<=128 THEN actor END FROM service_changes WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let rows = statement
            .query_map(params![after, i64::from(request.limit) + 1], |r| {
                Ok(Change {
                    sequence: r.get(0)?,
                    entity_kind: r.get(1)?,
                    entity_id: r.get(2)?,
                    revision: r.get(3)?,
                    actor: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(row_error)?;
        for (expected, row) in (after + 1..).zip(rows.iter()) {
            if row.sequence != expected {
                return Err(DataError::StaleCursor);
            }
        }
        if rows.is_empty() && after < current_sequence {
            return Err(DataError::StaleCursor);
        }
        let mut page = DesktopChangePage {
            items: Vec::new(),
            next: DesktopChangeCursor {
                library_scope: scope,
                after,
            },
            has_more: after < current_sequence,
            current_sequence,
        };
        for item in rows.into_iter().take(request.limit as usize) {
            page.next.after = item.sequence;
            page.items.push(item);
            page.has_more = page.next.after < current_sequence;
            if !fits(&page, request.max_bytes as usize) {
                page.items.pop();
                page.next.after = page.items.last().map_or(after, |change| change.sequence);
                page.has_more = page.next.after < current_sequence;
                if page.items.is_empty() {
                    return Err(DataError::Invalid("one desktop change exceeds page budget"));
                }
                break;
            }
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn fixture() -> Store {
        let store = Store::open_in_memory().unwrap();
        let exists: bool = store
            .conn
            .lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='service_desktop_state')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        if !exists {
            store
                .conn
                .lock()
                .execute_batch(include_str!("desktop.sql"))
                .unwrap();
        }
        store
    }
    fn ctx(key: &str) -> MutationContext {
        MutationContext {
            principal: "desktop".into(),
            idempotency_key: key.into(),
        }
    }
    fn new_meeting(store: &Store, key: &str, folder: Option<String>) -> MutationOutcome {
        store
            .service_create_text_meeting(
                &ctx(key),
                &NewTextMeeting {
                    title: format!("Meeting {key}"),
                    started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
                    participants: vec!["Анна".into()],
                    folder_id: folder,
                    tag_ids: vec![],
                    calendar_uid: None,
                    transcript: None,
                },
            )
            .unwrap()
    }
    #[test]
    fn preferences_are_narrow_and_preserve_capture_secrets_unknown_fields_and_retry() {
        let store = fixture();
        let original = serde_json::json!({"capture_system":false,"video_fps":30,"stt_provider":"soniox","soniox_consent":true,"llm_api_key":"DO-NOT-RETURN-THIS","private_future":{"a":[1,2]},"hotkey":"Alt+R","close_to_tray":false});
        store.set_setting("settings", &original).unwrap();
        let before = store.service_desktop_bootstrap().unwrap();
        assert_eq!(before.preferences.ui_locale, DesktopLocale::Ru);
        assert!(!before.preferences.close_to_tray);
        let response = serde_json::to_string(&before).unwrap();
        assert!(!response.contains("DO-NOT-RETURN"));
        assert!(!response.contains("stt_provider"));
        assert!(!response.contains("video_fps"));
        let patch = DesktopPreferencesPatch {
            ui_locale: Some(DesktopLocale::En),
            close_to_tray: Some(true),
            hotkey: Some("Ctrl+Shift+Q".into()),
        };
        let out = store
            .service_update_desktop_preferences(&ctx("prefs"), &before.preferences.revision, &patch)
            .unwrap();
        assert_eq!(
            out,
            store
                .service_update_desktop_preferences(
                    &ctx("prefs"),
                    &before.preferences.revision,
                    &patch
                )
                .unwrap()
        );
        assert!(matches!(
            store.service_update_desktop_preferences(
                &ctx("new-key"),
                &before.preferences.revision,
                &patch
            ),
            Err(DataError::Conflict { .. })
        ));
        let mut expected = original;
        expected["hotkey"] = "Ctrl+Shift+Q".into();
        expected["close_to_tray"] = true.into();
        assert_eq!(
            store
                .get_setting::<serde_json::Value>("settings")
                .unwrap()
                .unwrap(),
            expected
        );
        let after = store.service_desktop_preferences().unwrap();
        assert_eq!(after.revision, out.revision);
        assert_eq!(after.ui_locale, DesktopLocale::En);
        assert_eq!(
            store
                .service_desktop_changes(&DesktopChangesRequest::default())
                .unwrap()
                .items
                .len(),
            1
        );
        assert_eq!(out.entity_id, "desktop_preferences");
    }
    #[test]
    fn locale_policy_is_independent_and_malformed_settings_are_never_overwritten() {
        let store = fixture();
        let default = store.service_desktop_preferences().unwrap();
        assert_eq!(default.hotkey, "CmdOrCtrl+Shift+R");
        assert!(default.close_to_tray);
        for raw in ["\"de\"", "null", "{}", "[1,2]", &"x".repeat(100)] {
            store.conn.lock().execute("INSERT INTO settings(key,value) VALUES('ui_locale',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[raw]).unwrap();
            assert_eq!(
                store.service_desktop_preferences().unwrap().ui_locale,
                DesktopLocale::En
            );
        }
        let malformed = "{ deliberately malformed private capture settings";
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO settings(key,value) VALUES('settings',?1)",
                [malformed],
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_bootstrap(),
            Err(DataError::Invalid("malformed desktop settings"))
        ));
        let patch = DesktopPreferencesPatch {
            ui_locale: Some(DesktopLocale::Ru),
            ..Default::default()
        };
        let out = store
            .service_update_desktop_preferences(&ctx("locale"), &default.revision, &patch)
            .unwrap();
        let saved: String = store
            .conn
            .lock()
            .query_row("SELECT value FROM settings WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(saved, malformed);
        assert_eq!(
            store.get_setting::<DesktopLocale>("ui_locale").unwrap(),
            Some(DesktopLocale::Ru)
        );
        assert!(matches!(
            store.service_update_desktop_preferences(
                &ctx("hotkey"),
                &out.revision,
                &DesktopPreferencesPatch {
                    hotkey: Some("Alt+K".into()),
                    ..Default::default()
                }
            ),
            Err(DataError::Invalid("malformed desktop settings"))
        ));
    }
    #[test]
    fn preference_failure_rolls_back_settings_locale_revision_changes_and_receipt() {
        let store = fixture();
        store
            .set_setting(
                "settings",
                &serde_json::json!({"capture_system":false,"hotkey":"old"}),
            )
            .unwrap();
        let before = store.service_desktop_bootstrap().unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER injected_desktop_failure BEFORE INSERT ON service_changes BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
        let patch = DesktopPreferencesPatch {
            ui_locale: Some(DesktopLocale::En),
            hotkey: Some("new".into()),
            ..Default::default()
        };
        assert!(store
            .service_update_desktop_preferences(&ctx("retry"), &before.preferences.revision, &patch)
            .is_err());
        let after = store.service_desktop_bootstrap().unwrap();
        assert_eq!(before.preferences, after.preferences);
        assert_eq!(before.change_sequence, after.change_sequence);
        assert!(store
            .get_setting::<DesktopLocale>("ui_locale")
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_idempotency", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER injected_desktop_failure;")
            .unwrap();
        assert!(store
            .service_update_desktop_preferences(&ctx("retry"), &before.preferences.revision, &patch)
            .is_ok());
    }
    #[test]
    fn meeting_pages_obey_serialized_budget_and_reject_changed_snapshot_query_or_library() {
        let store = fixture();
        let mut expected = BTreeSet::new();
        for n in 0..120 {
            let outcome = new_meeting(&store, &format!("{n}"), None);
            store
                .conn
                .lock()
                .execute(
                    "UPDATE meetings SET title=?2 WHERE id=?1",
                    params![outcome.entity_id, format!("item{n}{}", "я".repeat(220))],
                )
                .unwrap();
            expected.insert(outcome.entity_id);
        }
        let request = DesktopMeetingRequest {
            limit: 100,
            max_bytes: 4096,
            ..Default::default()
        };
        let first = store.service_desktop_meetings(&request).unwrap();
        assert!(!first.items.is_empty());
        assert!(first.items.len() < 100);
        assert!(first.next.is_some());
        assert!(serde_json::to_vec(&first).unwrap().len() <= 4096);
        let saved = first.next.clone().unwrap();
        let mut found = BTreeSet::new();
        let mut request = request;
        loop {
            let page = store.service_desktop_meetings(&request).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
            for item in page.items {
                assert!(found.insert(item.id));
            }
            if let Some(next) = page.next {
                request.cursor = Some(next);
            } else {
                break;
            }
        }
        assert_eq!(found, expected);
        let wrong_query = DesktopMeetingRequest {
            cursor: Some(saved.clone()),
            filter: DesktopMeetingFilter {
                content_kind: Some("recorded".into()),
                ..Default::default()
            },
            ..request.clone()
        };
        assert!(matches!(
            store.service_desktop_meetings(&wrong_query),
            Err(DataError::StaleCursor)
        ));
        let other = fixture();
        assert!(matches!(
            other.service_desktop_meetings(&DesktopMeetingRequest {
                cursor: Some(saved.clone()),
                ..request.clone()
            }),
            Err(DataError::StaleCursor)
        ));
        let row = store
            .service_desktop_meeting(expected.first().unwrap())
            .unwrap()
            .unwrap();
        store
            .service_desktop_rename_meeting(
                &ctx("rename"),
                &row.id,
                &row.metadata_revision,
                "New title",
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_meetings(&DesktopMeetingRequest {
                cursor: Some(saved),
                ..request
            }),
            Err(DataError::StaleCursor)
        ));
    }

    #[test]
    fn invalid_lookahead_does_not_poison_valid_prefix_or_get_silently_skipped() {
        let store = fixture();
        let good = new_meeting(&store, "good-prefix", None);
        let bad = new_meeting(&store, "bad-lookahead", None);
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET started_at='2026-11-02T00:00:02Z' WHERE id=?1",
                [&good.entity_id],
            )
            .unwrap();
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET started_at='2026-11-02T00:00:01Z',title=?2 WHERE id=?1",
                params![bad.entity_id, "x".repeat(1_000_000)],
            )
            .unwrap();
        let request = DesktopMeetingRequest {
            limit: 1,
            ..Default::default()
        };
        let first = store.service_desktop_meetings(&request).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].id, good.entity_id);
        let next = first.next.unwrap();
        assert_eq!(next.offset, 1);
        assert!(matches!(
            store.service_desktop_meetings(&DesktopMeetingRequest {
                cursor: Some(next),
                ..request
            }),
            Err(DataError::Invalid(_))
        ));
        // The same valid prefix is returned if the bad row is inside this
        // requested item limit, rather than the pure one-row lookahead.
        let page = store
            .service_desktop_meetings(&DesktopMeetingRequest {
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.next.unwrap().offset, 1);
        assert_eq!(
            store
                .get_meeting(&bad.entity_id)
                .unwrap()
                .unwrap()
                .title
                .len(),
            1_000_000
        );
    }

    #[test]
    fn byte_limited_prefix_does_not_map_or_allocate_unused_later_headers() {
        let store = fixture();
        let mut good_ids = BTreeSet::new();
        for n in 0..5 {
            let good = new_meeting(&store, &format!("budget-prefix-{n}"), None);
            store
                .conn
                .lock()
                .execute(
                    "UPDATE meetings SET title=?2,started_at='2026-11-02T00:00:02Z' WHERE id=?1",
                    params![good.entity_id, "a".repeat(512)],
                )
                .unwrap();
            good_ids.insert(good.entity_id);
        }
        let bad = new_meeting(&store, "huge-unused-header", None);
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET title=?2,started_at='2026-11-02T00:00:01Z' WHERE id=?1",
                params![bad.entity_id, "x".repeat(1_000_000)],
            )
            .unwrap();
        let mut request = DesktopMeetingRequest {
            limit: 100,
            max_bytes: 4096,
            ..Default::default()
        };
        let first = store.service_desktop_meetings(&request).unwrap();
        assert!(!first.items.is_empty());
        assert!(first.items.len() < 5);
        assert!(serde_json::to_vec(&first).unwrap().len() <= 4096);
        let mut returned = BTreeSet::new();
        let mut page = first;
        loop {
            for item in page.items {
                assert!(returned.insert(item.id));
            }
            request.cursor = page.next;
            assert!(request.cursor.is_some());
            match store.service_desktop_meetings(&request) {
                Ok(next) => page = next,
                Err(DataError::Invalid(_)) => break,
                Err(error) => panic!("unexpected page failure: {error:?}"),
            }
        }
        assert_eq!(returned, good_ids);
        assert_eq!(request.cursor.unwrap().offset, 5);
    }
    #[test]
    fn subtree_unfiled_tag_kind_and_tombstone_filters_use_current_metadata() {
        let store = fixture();
        store.conn.lock().execute_batch("INSERT INTO folders(id,name,parent_id,created_at) VALUES('parent','Parent',NULL,'2026-11-02T00:00:00Z'),('child','Child','parent','2026-11-02T00:00:00Z');").unwrap();
        let parent = new_meeting(&store, "parent", Some("parent".into()));
        let child = new_meeting(&store, "child", Some("child".into()));
        let unfiled = new_meeting(&store, "none", None);
        let tag = store.tag_meeting(&child.entity_id, "focus").unwrap();
        let filtered = |filter| {
            store
                .service_desktop_meetings(&DesktopMeetingRequest {
                    filter,
                    ..Default::default()
                })
                .unwrap()
                .items
                .into_iter()
                .map(|m| m.id)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            filtered(DesktopMeetingFilter {
                folder: DesktopFolderFilter::Subtree {
                    folder_id: "parent".into()
                },
                ..Default::default()
            }),
            BTreeSet::from([parent.entity_id.clone(), child.entity_id.clone()])
        );
        assert_eq!(
            filtered(DesktopMeetingFilter {
                folder: DesktopFolderFilter::Unfiled,
                ..Default::default()
            }),
            BTreeSet::from([unfiled.entity_id])
        );
        assert_eq!(
            filtered(DesktopMeetingFilter {
                tag_id: Some(tag.id),
                ..Default::default()
            }),
            BTreeSet::from([child.entity_id])
        );
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_meeting_state SET content_kind='recorded' WHERE meeting_id=?1",
                [&parent.entity_id],
            )
            .unwrap();
        assert_eq!(
            filtered(DesktopMeetingFilter {
                content_kind: Some("recorded".into()),
                ..Default::default()
            }),
            BTreeSet::from([parent.entity_id.clone()])
        );
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_meeting_state SET tombstoned=1 WHERE meeting_id=?1",
                [&parent.entity_id],
            )
            .unwrap();
        assert!(store
            .service_desktop_meeting(&parent.entity_id)
            .unwrap()
            .is_none());
        assert!(filtered(DesktopMeetingFilter {
            content_kind: Some("recorded".into()),
            ..Default::default()
        })
        .is_empty());
    }
    #[test]
    fn metadata_rename_during_capture_or_processing_preserves_capture_fields_and_revisions() {
        let store = fixture();
        let outcome = new_meeting(&store, "rename", None);
        store.conn.lock().execute("UPDATE meetings SET duration_ms=1234,has_video=1,error='keep diagnostic',participants_json='[\"Sam\"]',ended_at='2026-11-02T00:00:03Z' WHERE id=?1",[&outcome.entity_id]).unwrap();
        for status in ["recording", "processing"] {
            store
                .conn
                .lock()
                .execute(
                    "UPDATE meetings SET status=?2 WHERE id=?1",
                    params![outcome.entity_id, status],
                )
                .unwrap();
            let old = store
                .service_desktop_meeting(&outcome.entity_id)
                .unwrap()
                .unwrap();
            let renamed = store
                .service_desktop_rename_meeting(
                    &ctx(status),
                    &old.id,
                    &old.metadata_revision,
                    "Kept my name",
                )
                .unwrap();
            let new = store.service_desktop_meeting(&old.id).unwrap().unwrap();
            assert_eq!(new.title, "Kept my name");
            assert_eq!(new.status, status);
            assert_eq!(new.duration_ms, 1234);
            assert!(new.has_video);
            assert_eq!(new.error.as_deref(), Some("keep diagnostic"));
            assert_eq!(new.participants, ["Sam"]);
            assert_eq!(new.metadata_revision, renamed.revision);
            assert_eq!(new.lifecycle_revision, old.lifecycle_revision);
            assert_eq!(new.transcript_revision, old.transcript_revision);
            assert_eq!(new.summary_revision, old.summary_revision);
            assert!(matches!(
                store.service_desktop_rename_meeting(
                    &ctx(&format!("stale-{status}")),
                    &old.id,
                    &old.metadata_revision,
                    "lost update"
                ),
                Err(DataError::Conflict { .. })
            ));
        }
    }

    #[test]
    fn opaque_legacy_meeting_ids_remain_readable_and_renameable() {
        let store = fixture();
        let mut meeting = crate::Meeting::new_recording("Imported legacy title", None);
        meeting.id = "legacy-import_42".into();
        meeting.status = crate::MeetingStatus::Ready;
        store.create_meeting(&meeting).unwrap();
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            meetings::initialize_meeting_on(&tx, &meeting.id, "recorded").unwrap();
            tx.commit().unwrap();
        }
        let before = store.service_desktop_meeting(&meeting.id).unwrap().unwrap();
        store
            .service_desktop_rename_meeting(
                &ctx("legacy-title"),
                &meeting.id,
                &before.metadata_revision,
                "Preserved legacy identity",
            )
            .unwrap();
        let after = store.service_desktop_meeting(&meeting.id).unwrap().unwrap();
        assert_eq!(after.id, "legacy-import_42");
        assert_eq!(after.title, "Preserved legacy identity");
    }

    #[test]
    fn desktop_append_preserves_legacy_equal_timestamp_tie_order() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!(
            "../../../bazza-service-test-support/src/schema_v8.sql"
        ))
        .unwrap();
        conn.execute_batch("CREATE INDEX meetings_folder ON meetings(folder_id);CREATE INDEX meeting_tags_tag ON meeting_tags(tag_id);PRAGMA user_version=9;INSERT INTO meetings(id,title,started_at,status,created_at) VALUES('z-first','First','2026-11-02T00:00:00Z','ready','2026-11-02T00:00:00Z'),('a-second','Second','2026-11-02T00:00:00Z','ready','2026-11-02T00:00:00Z');").unwrap();
        let ids = |conn: &Connection| {
            let mut statement = conn
                .prepare("SELECT id FROM meetings ORDER BY started_at DESC")
                .unwrap();
            let rows = statement
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows
        };
        let before = ids(&conn);
        assert_eq!(before, ["z-first", "a-second"]);
        crate::migrations::run(&conn).unwrap();
        assert_eq!(ids(&conn), before);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='service_desktop_meeting_order'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn detail_error_is_truthfully_bounded_and_oversized_legacy_title_is_not_changed() {
        let store = fixture();
        let outcome = new_meeting(&store, "large", None);
        let error = "я".repeat(3000);
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET error=?2 WHERE id=?1",
                params![outcome.entity_id, error],
            )
            .unwrap();
        let row = store
            .service_desktop_meeting(&outcome.entity_id)
            .unwrap()
            .unwrap();
        assert!(row.error_truncated);
        assert_eq!(row.error.unwrap().len(), 2048);
        assert_eq!(
            store
                .get_meeting(&outcome.entity_id)
                .unwrap()
                .unwrap()
                .error
                .unwrap(),
            error
        );
        let title = "x".repeat(1_000_000);
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET title=?2 WHERE id=?1",
                params![outcome.entity_id, title],
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_meeting(&outcome.entity_id),
            Err(DataError::Invalid(_))
        ));
        assert!(matches!(
            store.service_desktop_meetings(&DesktopMeetingRequest::default()),
            Err(DataError::Invalid(_))
        ));
        assert_eq!(
            store
                .get_meeting(&outcome.entity_id)
                .unwrap()
                .unwrap()
                .title,
            title
        );
    }
    #[test]
    fn media_flags_do_not_create_directories_or_invent_media_for_text_meetings() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("bazza.db")).unwrap();
        if store.service_desktop_bootstrap().is_err() {
            store
                .conn
                .lock()
                .execute_batch(include_str!("desktop.sql"))
                .unwrap();
        }
        let outcome = new_meeting(&store, "media", None);
        let before = store
            .service_desktop_meeting(&outcome.entity_id)
            .unwrap()
            .unwrap();
        assert!(!before.media.audio);
        assert!(!temp.path().join("recordings").exists());
        let path = temp.path().join("recordings").join(&outcome.entity_id);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("mix.wav"), b"synthetic").unwrap();
        std::fs::write(path.join("screen.mp4"), b"synthetic").unwrap();
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET has_video=1 WHERE id=?1",
                [&outcome.entity_id],
            )
            .unwrap();
        assert_eq!(
            store
                .service_desktop_meeting(&outcome.entity_id)
                .unwrap()
                .unwrap()
                .media,
            DesktopMedia::default()
        );
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_meeting_state SET content_kind='recorded' WHERE meeting_id=?1",
                [&outcome.entity_id],
            )
            .unwrap();
        assert_eq!(
            store
                .service_desktop_meeting(&outcome.entity_id)
                .unwrap()
                .unwrap()
                .media,
            DesktopMedia {
                audio: true,
                video: true
            }
        );
        std::fs::remove_file(path.join("mix.wav")).unwrap();
        assert!(
            !store
                .service_desktop_meeting(&outcome.entity_id)
                .unwrap()
                .unwrap()
                .media
                .audio
        );
    }
    #[test]
    fn change_pages_obey_budget_resume_consecutively_and_reject_gaps_and_foreign_scope() {
        let store = fixture();
        for n in 0..12 {
            new_meeting(&store, &n.to_string(), None);
        }
        let mut request = DesktopChangesRequest {
            limit: 100,
            max_bytes: 1024,
            ..Default::default()
        };
        let mut sequences = Vec::new();
        loop {
            let page = store.service_desktop_changes(&request).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= 1024);
            sequences.extend(page.items.iter().map(|c| c.sequence));
            request.cursor = Some(page.next);
            if !page.has_more {
                break;
            }
        }
        assert_eq!(sequences, (1..=12).collect::<Vec<_>>());
        let other = fixture();
        assert!(matches!(
            other.service_desktop_changes(&request),
            Err(DataError::StaleCursor)
        ));
        store
            .conn
            .lock()
            .execute("DELETE FROM service_changes WHERE sequence=2", [])
            .unwrap();
        assert!(matches!(
            store.service_desktop_changes(&DesktopChangesRequest::default()),
            Err(DataError::StaleCursor)
        ));
        let scope = store.service_desktop_bootstrap().unwrap().cursor_scope;
        assert!(matches!(
            store.service_desktop_changes(&DesktopChangesRequest {
                cursor: Some(DesktopChangeCursor {
                    library_scope: scope.clone(),
                    after: 1
                }),
                ..Default::default()
            }),
            Err(DataError::StaleCursor)
        ));
        store
            .conn
            .lock()
            .execute("DELETE FROM service_changes", [])
            .unwrap();
        let bootstrap = store.service_desktop_bootstrap().unwrap();
        assert_eq!(bootstrap.change_sequence, 12);
        let page = store
            .service_desktop_changes(&DesktopChangesRequest {
                cursor: Some(DesktopChangeCursor {
                    library_scope: scope,
                    after: 12,
                }),
                ..Default::default()
            })
            .unwrap();
        assert!(!page.has_more);
        assert!(page.items.is_empty());
    }
}
