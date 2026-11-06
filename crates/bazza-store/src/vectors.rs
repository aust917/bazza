//! Chunk embeddings for semantic retrieval, plus BM25 + vector rank fusion.

use crate::ai::ChunkHit;
use crate::{cosine, Result, Store};
use rusqlite::{params, OptionalExtension};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

const VECTOR_BATCH_SIZE: usize = 512;
const SCOPED_VECTOR_BATCH_SQL: &str =
    "SELECT c.id, c.meeting_id, v.vec FROM chunks c JOIN chunk_vectors v ON v.chunk_id = c.id
     WHERE c.meeting_id = ?1 AND v.dim = ?2 AND c.id >= ?3 ORDER BY c.id LIMIT ?4";
const ALL_VECTOR_BATCH_SQL: &str =
    "SELECT c.id, c.meeting_id, v.vec FROM chunk_vectors v JOIN chunks c ON c.id = v.chunk_id
     WHERE v.dim = ?1 AND v.chunk_id >= ?2 ORDER BY v.chunk_id LIMIT ?3";

struct VectorCandidate {
    id: i64,
    meeting_id: String,
    bytes: Vec<u8>,
}

struct ScoredVector {
    candidate: VectorCandidate,
    rank: f64,
}

// The heap keeps its worst result at the front. On equal scores, preserve the
// original vector row-ID order rather than the order of a scoped SQL index.
impl Ord for ScoredVector {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank.partial_cmp(&other.rank).unwrap_or_else(|| self.rank.total_cmp(&other.rank))
            .then_with(|| self.candidate.id.cmp(&other.candidate.id))
    }
}
impl PartialOrd for ScoredVector {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl PartialEq for ScoredVector {
    fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal }
}
impl Eq for ScoredVector {}

fn blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn unblob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

impl Store {
    /// Ignore a stale inference result if chunks were rebuilt or edited while
    /// the model was running, including SQLite row-ID reuse after deletion.
    pub fn set_chunk_vectors_if_current(&self, meeting_id: &str, rows: &[(i64, String, Vec<f32>)]) -> Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let mut count = 0;
        {
            let mut statement = tx.prepare(
                "INSERT OR REPLACE INTO chunk_vectors(chunk_id,dim,vec) SELECT id,?4,?5 FROM chunks WHERE meeting_id=?1 AND id=?2 AND text=?3",
            )?;
            for (id, text, vector) in rows { count += statement.execute(params![meeting_id, id, text, vector.len() as i64, blob(vector)])?; }
        }
        tx.commit()?;
        Ok(count)
    }

    pub fn set_chunk_vectors(&self, rows: &[(i64, Vec<f32>)]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare("INSERT OR REPLACE INTO chunk_vectors(chunk_id, dim, vec) VALUES (?1, ?2, ?3)")?;
            for (id, v) in rows {
                st.execute(params![id, v.len() as i64, blob(v)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Chunks of a meeting that have no embedding yet: (chunk_id, text).
    pub fn chunks_without_vectors(&self, meeting_id: &str) -> Result<Vec<(i64, String)>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT c.id, c.text FROM chunks c LEFT JOIN chunk_vectors v ON v.chunk_id = c.id
             WHERE c.meeting_id = ?1 AND v.chunk_id IS NULL ORDER BY c.start_ms",
        )?;
        let rows = st.query_map(params![meeting_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn has_chunk_vectors(&self, meeting_id: &str) -> Result<bool> {
        let n: i64 = self.conn.lock().query_row(
            "SELECT COUNT(*) FROM chunk_vectors v JOIN chunks c ON c.id = v.chunk_id WHERE c.meeting_id = ?1",
            params![meeting_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Nearest chunks by cosine similarity, with bounded batches and top-k storage.
    /// Only the winning chunks' text is read; scoring never holds the database mutex.
    pub fn search_chunks_vec(&self, query: &[f32], scope: Option<&str>, limit: usize) -> Result<Vec<ChunkHit>> {
        self.search_chunks_vec_with_score(query, scope, limit, cosine)
    }

    fn vector_batch(&self, scope: Option<&str>, dimensions: usize, next_id: i64) -> Result<Vec<VectorCandidate>> {
        let conn = self.conn.lock();
        let from_row = |row: &rusqlite::Row<'_>| Ok(VectorCandidate {
            id: row.get(0)?, meeting_id: row.get(1)?, bytes: row.get(2)?,
        });
        let rows = if let Some(meeting_id) = scope {
            let mut statement = conn.prepare(SCOPED_VECTOR_BATCH_SQL)?;
            let rows = statement.query_map(params![meeting_id, dimensions as i64, next_id, VECTOR_BATCH_SIZE as i64], from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            let mut statement = conn.prepare(ALL_VECTOR_BATCH_SQL)?;
            let rows = statement.query_map(params![dimensions as i64, next_id, VECTOR_BATCH_SIZE as i64], from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    fn search_chunks_vec_with_score(
        &self,
        query: &[f32],
        scope: Option<&str>,
        limit: usize,
        mut score: impl FnMut(&[f32], &[f32]) -> f32,
    ) -> Result<Vec<ChunkHit>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut best = BinaryHeap::<ScoredVector>::new();
        let mut next_id = i64::MIN;
        loop {
            let batch = self.vector_batch(scope, query.len(), next_id)?;
            let Some(last) = batch.last() else { break };
            let following_id = last.id.checked_add(1);
            let done = batch.len() < VECTOR_BATCH_SIZE || following_id.is_none();
            // vector_batch has returned owned bytes and released its guard.
            for candidate in batch {
                let rank = -(score(query, &unblob(&candidate.bytes)) as f64);
                let candidate = ScoredVector { candidate, rank };
                if best.len() < limit {
                    best.push(candidate);
                } else if best.peek().is_some_and(|worst| candidate < *worst) {
                    *best.peek_mut().expect("nonempty top-k heap") = candidate;
                }
            }
            if done { break; }
            next_id = following_id.expect("unfinished batch has a following row ID");
        }
        let best = best.into_sorted_vec();
        let conn = self.conn.lock();
        let mut statement = conn.prepare(
            "SELECT c.meeting_id, m.title, m.started_at, c.start_ms, c.end_ms, c.text
             FROM chunk_vectors v JOIN chunks c ON c.id = v.chunk_id JOIN meetings m ON m.id = c.meeting_id
             WHERE c.id = ?1 AND c.meeting_id = ?2 AND v.dim = ?3 AND v.vec = ?4",
        )?;
        let mut hits = Vec::with_capacity(best.len());
        for scored in best {
            // A correction can rebuild chunks while scoring is running, including
            // reusing row IDs. Never attach an old score to a replacement vector.
            let hit = statement.query_row(
                params![scored.candidate.id, scored.candidate.meeting_id, query.len() as i64, scored.candidate.bytes],
                |row| Ok(ChunkHit {
                    meeting_id: row.get(0)?, meeting_title: row.get(1)?, started_at: row.get(2)?,
                    start_ms: row.get(3)?, end_ms: row.get(4)?, text: row.get(5)?, rank: scored.rank,
                }),
            ).optional()?;
            if let Some(hit) = hit { hits.push(hit); }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Meeting, NewSegment};

    #[test]
    fn performance_scoped_retrieval_scores_only_matching_vectors_across_bounded_batches() {
        let store = Store::open_in_memory().unwrap();
        let wanted = Meeting::new_recording("Selected meeting", None);
        let unrelated = Meeting::new_recording("Unrelated meeting", None);
        store.create_meeting(&wanted).unwrap();
        store.create_meeting(&unrelated).unwrap();
        let mut matching = Vec::new();
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            // More than two eligible batches, interspersed with another meeting,
            // incompatible models, and missing IDs. An unscoped scan can return
            // identical winners while wasting most of its scoring work.
            for index in 0..VECTOR_BATCH_SIZE * 6 + 17 {
                let id = index as i64 * 3 + 7;
                let selected = index % 2 == 0;
                let meeting_id = if selected { &wanted.id } else { &unrelated.id };
                tx.execute("INSERT INTO chunks(id,meeting_id,start_ms,end_ms,text) VALUES (?1,?2,?3,?4,?5)",
                    params![id, meeting_id, id * 1000, id * 1000 + 500, format!("candidate {id}")]).unwrap();
                let vector = if index % 3 == 0 { vec![0.0, 0.0, 1.0] }
                    else { vec![index as f32, if selected { 1.0 } else { -1.0 }] };
                tx.execute("INSERT INTO chunk_vectors(chunk_id,dim,vec) VALUES (?1,?2,?3)",
                    params![id, vector.len() as i64, blob(&vector)]).unwrap();
                if selected && vector.len() == 2 { matching.push(id); }
            }
            tx.commit().unwrap();
        }
        assert!(matching.len() > VECTOR_BATCH_SIZE * 2);

        let mut seen = Vec::new();
        let mut next_id = i64::MIN;
        loop {
            let batch = store.vector_batch(Some(&wanted.id), 2, next_id).unwrap();
            if batch.is_empty() { break; }
            assert!(batch.len() <= 512, "one retrieval read must remain bounded regardless of library size");
            next_id = batch.last().unwrap().id + 1;
            seen.extend(batch.into_iter().map(|candidate| candidate.id));
        }
        assert_eq!(seen, matching, "keyset batches must neither repeat nor skip sparse candidate IDs");

        let mut scored = 0;
        let hits = store.search_chunks_vec_with_score(&[1.0, 0.0], Some(&wanted.id), 7, |query, vector| {
            scored += 1;
            assert_eq!(vector.len(), query.len(), "incompatible model dimensions must be filtered before scoring");
            assert_eq!(vector[1], 1.0, "another meeting must not consume scoped retrieval scoring work");
            cosine(query, vector)
        }).unwrap();
        assert_eq!(scored, matching.len(), "each eligible vector must be scored exactly once");
        let expected: Vec<_> = matching.iter().rev().take(7).map(|id| format!("candidate {id}")).collect();
        assert_eq!(hits.iter().map(|hit| hit.text.clone()).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn stale_vectors_cannot_attach_to_reused_chunk_ids_after_correction() {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("vector correction", None); meeting.status = crate::MeetingStatus::Ready;
        store.create_meeting(&meeting).unwrap();
        store.replace_segments(&meeting.id, &[NewSegment {start_ms:0,end_ms:1000,speaker:None,text:"old retrieval text".into()}]).unwrap();
        store.rebuild_chunks(&meeting.id).unwrap();
        let old = store.chunks_without_vectors(&meeting.id).unwrap();
        let segment = store.segments(&meeting.id).unwrap().remove(0);
        store.set_transcript_correction(&meeting.id, segment.id, Some("new corrected text")).unwrap();
        let current = store.chunks_without_vectors(&meeting.id).unwrap();
        assert_eq!(current[0].0, old[0].0, "exercise SQLite row-ID reuse explicitly");
        assert_eq!(store.set_chunk_vectors_if_current(&meeting.id, &[(old[0].0,old[0].1.clone(),vec![1.0,0.0])]).unwrap(), 0);
        assert!(!store.has_chunk_vectors(&meeting.id).unwrap());
        assert_eq!(store.set_chunk_vectors_if_current("another-meeting", &[(current[0].0,current[0].1.clone(),vec![0.0,1.0])]).unwrap(), 0);
        assert_eq!(store.set_chunk_vectors_if_current(&meeting.id, &[(current[0].0,current[0].1.clone(),vec![0.0,1.0])]).unwrap(), 1);
        let hits = store.search_chunks_vec(&[0.0,1.0], Some(&meeting.id), 5).unwrap();
        assert_eq!(hits.len(), 1); assert_eq!(hits[0].text, "new corrected text");
    }

    #[test]
    fn vector_search_ranks_nearest_first() {
        let s = Store::open_in_memory().unwrap();
        let m = Meeting::new_recording("m", None);
        s.create_meeting(&m).unwrap();
        s.replace_segments(
            &m.id,
            &[
                NewSegment { start_ms: 0, end_ms: 1000, speaker: None, text: "pricing".into() },
                NewSegment { start_ms: 60_000, end_ms: 61_000, speaker: None, text: "hiring".into() },
            ],
        )
        .unwrap();
        s.rebuild_chunks(&m.id).unwrap();
        let todo = s.chunks_without_vectors(&m.id).unwrap();
        assert_eq!(todo.len(), 2);
        assert!(!s.has_chunk_vectors(&m.id).unwrap());
        s.set_chunk_vectors(&[(todo[0].0, vec![1.0, 0.0]), (todo[1].0, vec![0.0, 1.0])]).unwrap();
        assert!(s.has_chunk_vectors(&m.id).unwrap());
        assert!(s.chunks_without_vectors(&m.id).unwrap().is_empty());
        let hits = s.search_chunks_vec(&[0.1, 0.9], Some(&m.id), 5).unwrap();
        assert_eq!(hits[0].text, "hiring");
        assert_eq!(hits.len(), 2);
        assert!(s.search_chunks_vec(&[1.0, 0.0, 0.0], None, 5).unwrap().is_empty()); // dim mismatch
        // Rebuilding chunks drops stale vectors (cascade).
        s.rebuild_chunks(&m.id).unwrap();
        assert!(!s.has_chunk_vectors(&m.id).unwrap());
    }

    #[test]
    fn batched_top_k_matches_full_sort_for_global_and_scoped_queries_including_ties() {
        let store = Store::open_in_memory().unwrap();
        let meetings = [Meeting::new_recording("First", None), Meeting::new_recording("Second", None)];
        for meeting in &meetings { store.create_meeting(meeting).unwrap(); }
        let count = VECTOR_BATCH_SIZE * 2 + 17;
        let mut expected = Vec::new();
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            for i in 0..count {
                let id = i as i64 + 1;
                let meeting_id = &meetings[i % 2].id;
                let text = format!("candidate {id}");
                // Deliberately reverse timestamps so index order differs from row IDs.
                let start = (count - i) as i64 * 45_000;
                tx.execute("INSERT INTO chunks(id,meeting_id,start_ms,end_ms,text) VALUES (?1,?2,?3,?4,?5)", params![id,meeting_id,start,start+1000,text]).unwrap();
                let vector = if i % 7 == 0 { vec![1.0,0.0,0.0] } else { vec![(i % 5) as f32 / 4.0,0.0] };
                tx.execute("INSERT INTO chunk_vectors(chunk_id,dim,vec) VALUES (?1,?2,?3)", params![id,vector.len() as i64,blob(&vector)]).unwrap();
                if vector.len() == 2 { expected.push((-(cosine(&[1.0,0.0], &vector) as f64),id,meeting_id.clone(),text)); }
            }
            tx.commit().unwrap();
        }
        // Full stable sorting is the previous implementation's ranking baseline.
        expected.sort_by(|a,b| a.0.partial_cmp(&b.0).unwrap());
        for scope in [None, Some(meetings[0].id.as_str()), Some(meetings[1].id.as_str())] {
            for limit in [0, 1, 12, VECTOR_BATCH_SIZE + 2, count + 1] {
                let wanted: Vec<_> = expected.iter().filter(|item| scope.is_none_or(|id| item.2 == id)).take(limit).map(|item| (item.3.as_str(),item.0)).collect();
                let hits = store.search_chunks_vec(&[1.0,0.0], scope, limit).unwrap();
                let actual: Vec<_> = hits.iter().map(|hit| (hit.text.as_str(),hit.rank)).collect();
                assert_eq!(actual, wanted, "scope={scope:?}, limit={limit}");
            }
        }
    }

    #[test]
    fn vector_scoring_allows_another_thread_to_write_to_the_store() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        let store = Arc::new(Store::open_in_memory().unwrap());
        let meeting = Meeting::new_recording("Concurrent retrieval", None);
        store.create_meeting(&meeting).unwrap();
        store.replace_segments(&meeting.id, &[NewSegment { start_ms:0,end_ms:1000,speaker:None,text:"available while scoring".into() }]).unwrap();
        store.rebuild_chunks(&meeting.id).unwrap();
        let id = store.chunks_without_vectors(&meeting.id).unwrap()[0].0;
        store.set_chunk_vectors(&[(id,vec![1.0,0.0])]).unwrap();
        let (start_tx,start_rx) = mpsc::channel();
        let (done_tx,done_rx) = mpsc::channel();
        let writer_store = store.clone();
        let writer = std::thread::spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            writer_store.set_setting("concurrent-vector-write", &true).unwrap();
            done_tx.send(()).unwrap();
        });
        let mut start_tx = Some(start_tx);
        let hits = store.search_chunks_vec_with_score(&[1.0,0.0], None, 1, |query,vector| {
            if let Some(start) = start_tx.take() {
                start.send(()).unwrap();
                done_rx.recv_timeout(Duration::from_secs(2)).expect("vector scoring must release the Store mutex");
            }
            cosine(query,vector)
        }).unwrap();
        writer.join().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(store.get_setting::<bool>("concurrent-vector-write").unwrap(), Some(true));
    }

    #[test]
    fn vector_result_revalidates_a_chunk_rebuilt_while_scoring() {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("Edited during retrieval", None);
        meeting.status = crate::MeetingStatus::Ready;
        store.create_meeting(&meeting).unwrap();
        store.replace_segments(&meeting.id, &[NewSegment {start_ms:0,end_ms:1000,speaker:None,text:"old text".into()}]).unwrap();
        store.rebuild_chunks(&meeting.id).unwrap();
        let old_id = store.chunks_without_vectors(&meeting.id).unwrap()[0].0;
        store.set_chunk_vectors(&[(old_id,vec![1.0,0.0])]).unwrap();
        let segment_id = store.segments(&meeting.id).unwrap()[0].id;
        let hits = store.search_chunks_vec_with_score(&[1.0,0.0], Some(&meeting.id), 1, |query,vector| {
            store.set_transcript_correction(&meeting.id, segment_id, Some("new text")).unwrap();
            let new_id = store.chunks_without_vectors(&meeting.id).unwrap()[0].0;
            assert_eq!(old_id, new_id, "exercise row-ID reuse");
            store.set_chunk_vectors(&[(new_id,vec![0.0,1.0])]).unwrap();
            cosine(query,vector)
        }).unwrap();
        assert!(hits.is_empty(), "an old score must not attach to newly edited text");
        let current = store.search_chunks_vec(&[0.0,1.0], Some(&meeting.id), 1).unwrap();
        assert_eq!(current[0].text, "new text");
    }

    #[test]
    fn scoped_vector_batch_uses_the_meeting_index() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn.lock();
        let plan: Vec<String> = conn.prepare(&format!("EXPLAIN QUERY PLAN {SCOPED_VECTOR_BATCH_SQL}")).unwrap()
            .query_map(params!["meeting",384,i64::MIN,VECTOR_BATCH_SIZE as i64], |row| row.get(3)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap();
        assert!(plan.iter().any(|line| line.contains("chunks_meeting") && line.contains("meeting_id=?")), "{plan:?}");
        assert!(!plan.iter().any(|line| line.contains("SCAN v")), "{plan:?}");
    }
}
