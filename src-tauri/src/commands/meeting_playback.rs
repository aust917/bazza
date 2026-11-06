//! Main-window-only playback of fixed, pinned files from the selected library.
use super::Media;
use bazza_media_gate::{MediaReadGuard, PlaybackMediaKind};
use http_range::HttpRange;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tauri::{
    http::{header, Method, Request, Response, StatusCode},
    Manager, Runtime, UriSchemeContext, UriSchemeResponder,
};
use uuid::Uuid;

const MAX_ACTIVE_PINS: usize = 32;
const MAX_IN_FLIGHT: usize = 8;
const MAX_CHUNK: u64 = 4 * 1024 * 1024;

#[derive(Clone, Default)]
pub(crate) struct ServicePlayback(Arc<PlaybackRegistry>);

#[derive(Default)]
struct PlaybackRegistry {
    entries: Mutex<HashMap<Uuid, Arc<PlaybackFiles>>>,
    in_flight: AtomicUsize,
}

pub(super) struct PlaybackFiles {
    _pin: MediaReadGuard,
    audio: Option<Mutex<File>>,
    video: Option<Mutex<File>>,
}

impl PlaybackFiles {
    pub(super) fn open(root: &Path, id: &str, audio: bool, video: bool) -> Option<Self> {
        let pin = MediaReadGuard::acquire(root, id).ok()?;
        let audio = audio
            .then(|| pin.open_playback_file(PlaybackMediaKind::Audio).ok())
            .flatten()
            .map(Mutex::new);
        let video = video
            .then(|| pin.open_playback_file(PlaybackMediaKind::Video).ok())
            .flatten()
            .map(Mutex::new);
        (audio.is_some() || video.is_some()).then_some(Self {
            _pin: pin,
            audio,
            video,
        })
    }
}

impl ServicePlayback {
    pub(super) fn register(&self, files: PlaybackFiles) -> Option<Media> {
        let mut entries = self.0.entries.lock();
        if entries.len() >= MAX_ACTIVE_PINS {
            return None;
        }
        let token = Uuid::new_v4();
        let media = Media {
            audio: files
                .audio
                .is_some()
                .then(|| format!("bazza-media:{token}-audio")),
            video: files
                .video
                .is_some()
                .then(|| format!("bazza-media:{token}-video")),
            lease: Some(token),
        };
        entries.insert(token, Arc::new(files));
        Some(media)
    }

    pub(super) fn release(&self, token: Uuid) {
        self.0.entries.lock().remove(&token);
    }
}

struct ReadSlot(Arc<PlaybackRegistry>);
impl ReadSlot {
    fn acquire(registry: &Arc<PlaybackRegistry>) -> Option<Self> {
        registry
            .in_flight
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAX_IN_FLIGHT).then_some(current + 1)
            })
            .ok()?;
        Some(Self(registry.clone()))
    }
}
impl Drop for ReadSlot {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Release);
    }
}

pub(crate) fn serve_service_playback<R: Runtime>(
    context: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let registry = context.app_handle().try_state::<ServicePlayback>();
    let entry = request
        .uri()
        .path()
        .strip_prefix('/')
        .and_then(|value| value.rsplit_once('-'));
    let target = entry.and_then(|(id, kind)| {
        if request.uri().query().is_some() || context.webview_label() != "main" {
            return None;
        }
        let token = Uuid::parse_str(id)
            .ok()
            .filter(|token| token.to_string() == id)?;
        let kind = match kind {
            "audio" => PlaybackMediaKind::Audio,
            "video" => PlaybackMediaKind::Video,
            _ => return None,
        };
        let registry = registry.as_ref()?;
        let files = registry.0.entries.lock().get(&token).cloned()?;
        Some((files, kind))
    });
    let Some((files, kind)) = target else {
        responder.respond(empty(StatusCode::NOT_FOUND));
        return;
    };
    let Some(slot) = registry
        .as_ref()
        .and_then(|registry| ReadSlot::acquire(&registry.0))
    else {
        responder.respond(empty(StatusCode::SERVICE_UNAVAILABLE));
        return;
    };
    drop(tauri::async_runtime::spawn_blocking(move || {
        let (file, mime) = match kind {
            PlaybackMediaKind::Audio => (files.audio.as_ref(), "audio/wav"),
            PlaybackMediaKind::Video => (files.video.as_ref(), "video/mp4"),
        };
        let response = file.map_or_else(
            || empty(StatusCode::NOT_FOUND),
            |file| read_response(&mut file.lock(), mime, &request),
        );
        responder.respond(response);
        drop(slot);
    }));
}

fn empty(status: StatusCode) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Vec::new())
        .expect("static playback response")
}

fn read_response(file: &mut File, mime: &str, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let head = request.method() == Method::HEAD;
    if !head && request.method() != Method::GET {
        return empty(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Ok(metadata) = file.metadata() else {
        return empty(StatusCode::NOT_FOUND);
    };
    let len = metadata.len();
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store");
    let unsatisfiable = || {
        Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .header(header::CACHE_CONTROL, "no-store")
            .body(Vec::new())
            .expect("static playback error")
    };
    let (start, count) = if let Some(range) = request.headers().get(header::RANGE) {
        let Some(ranges) = range
            .to_str()
            .ok()
            .filter(|range| range.len() <= 128)
            .and_then(|range| HttpRange::parse(range, len).ok())
        else {
            return unsatisfiable();
        };
        let [range] = ranges.as_slice() else {
            return unsatisfiable();
        };
        let count = range.length.min(MAX_CHUNK);
        if count == 0 {
            return unsatisfiable();
        }
        response = response.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{len}", range.start, range.start + count - 1),
        );
        (range.start, count)
    } else {
        if !head && len > MAX_CHUNK {
            return unsatisfiable();
        }
        (0, len)
    };
    let mut body = Vec::new();
    if !head {
        body.resize(count as usize, 0);
        if file
            .seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut body))
            .is_err()
        {
            return empty(StatusCode::NOT_FOUND);
        }
    }
    response
        .header(header::CONTENT_LENGTH, count)
        .body(body)
        .expect("validated playback response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_media_gate::{MediaGateError, MediaPurgeGuard};

    #[test]
    fn response_reads_a_bounded_range_and_pins_until_the_last_read_finishes() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("recordings").join("meeting");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mix.wav"), b"0123456789").unwrap();
        let state = ServicePlayback::default();
        let files = PlaybackFiles::open(root.path(), "meeting", true, false).unwrap();
        let media = state.register(files).unwrap();
        let token = media.lease.unwrap();
        let active_read = state.0.entries.lock().get(&token).cloned().unwrap();
        state.release(token);
        assert!(matches!(
            MediaPurgeGuard::acquire(root.path(), "meeting"),
            Err(MediaGateError::Busy)
        ));
        let request = Request::builder()
            .header(header::RANGE, "bytes=3-5")
            .body(Vec::new())
            .unwrap();
        let response = read_response(
            &mut active_read.audio.as_ref().unwrap().lock(),
            "audio/wav",
            &request,
        );
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 3-5/10");
        assert_eq!(response.body(), b"345");
        drop(active_read);
        assert!(MediaPurgeGuard::acquire(root.path(), "meeting").is_ok());
    }
}
