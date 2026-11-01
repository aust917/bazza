//! Strict, bounded subtitle parsing for importing existing transcripts.

use crate::{Result, ShareError};
use hark_store::NewSegment;

pub const MAX_SUBTITLE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CUES: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitleFormat { Srt, Vtt }

fn invalid(message: impl Into<String>) -> ShareError { ShareError::Other(message.into()) }

fn timestamp(value: &str, format: SubtitleFormat) -> Result<i64> {
    let separator = if format == SubtitleFormat::Srt { ',' } else { '.' };
    let (time, fraction) = value.split_once(separator).ok_or_else(|| invalid("timestamp needs milliseconds"))?;
    if fraction.len() != 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("timestamp milliseconds must have three digits"));
    }
    let parts: Vec<_> = time.split(':').collect();
    if parts.len() != 3 && !(format == SubtitleFormat::Vtt && parts.len() == 2) {
        return Err(invalid("invalid subtitle timestamp"));
    }
    if parts.iter().any(|p| p.len() < 2 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return Err(invalid("invalid subtitle timestamp digits"));
    }
    if parts[parts.len()-2..].iter().any(|p| p.len() != 2) {
        return Err(invalid("subtitle minutes and seconds must have two digits"));
    }
    let number = |s: &str| s.parse::<i64>().map_err(|_| invalid("subtitle timestamp overflow"));
    let (hours, minutes, seconds) = if parts.len() == 3 {
        (number(parts[0])?,number(parts[1])?,number(parts[2])?)
    } else { (0,number(parts[0])?,number(parts[1])?) };
    if minutes > 59 || seconds > 59 { return Err(invalid("subtitle minutes and seconds must be below 60")); }
    hours.checked_mul(3_600_000)
        .and_then(|n| n.checked_add(minutes * 60_000 + seconds * 1000))
        .and_then(|n| n.checked_add(number(fraction).ok()?))
        .ok_or_else(|| invalid("subtitle timestamp overflow"))
}

/// SRT keeps speaker prefixes in the original text. WebVTT voice spans map to
/// speaker labels; a cue containing several voices is rejected rather than
/// silently assigning all its words to one person.
pub fn parse_subtitles(text: &str, format: SubtitleFormat) -> Result<Vec<NewSegment>> {
    if text.len() > MAX_SUBTITLE_BYTES { return Err(invalid("transcript exceeds 16 MiB")); }
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n","\n").replace('\r',"\n");
    let mut lines = normalized.lines().peekable();
    if format == SubtitleFormat::Vtt {
        let header = lines.next().ok_or_else(|| invalid("empty WebVTT file"))?;
        if header != "WEBVTT" && !header.starts_with("WEBVTT ") && !header.starts_with("WEBVTT\t") {
            return Err(invalid("WebVTT file must start with WEBVTT"));
        }
        while lines.peek().is_some_and(|l| !l.trim().is_empty()) { lines.next(); }
    }
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            if !current.is_empty() { blocks.push(std::mem::take(&mut current)); }
        } else { current.push(line); }
    }
    if !current.is_empty() { blocks.push(current); }
    let mut segments: Vec<NewSegment> = Vec::new();
    for block in blocks {
        if format == SubtitleFormat::Vtt && (block[0] == "NOTE" || block[0].starts_with("NOTE ") || ["STYLE","REGION"].contains(&block[0])) {
            continue;
        }
        let cue = segments.len() + 1;
        let time_index = if block[0].contains("-->") {0} else {1};
        let timing = block.get(time_index).ok_or_else(|| invalid(format!("cue {cue} has no timing line")))?;
        let (start, end) = timing.split_once("-->").ok_or_else(|| invalid(format!("cue {cue} has no timing separator")))?;
        let end = end.split_whitespace().next().ok_or_else(|| invalid(format!("cue {cue} has no end time")))?;
        let start_ms = timestamp(start.trim(),format).map_err(|e| invalid(format!("cue {cue}: {e}")))?;
        let end_ms = timestamp(end,format).map_err(|e| invalid(format!("cue {cue}: {e}")))?;
        if end_ms <= start_ms || segments.last().is_some_and(|s| s.start_ms > start_ms) {
            return Err(invalid(format!("cue {cue} must have positive duration and chronological start time")));
        }
        let mut body = block[time_index+1..].join("\n");
        if body.trim().is_empty() { return Err(invalid(format!("cue {cue} has no text"))); }
        let mut speaker = None;
        if format == SubtitleFormat::Vtt && body.starts_with("<v ") {
            let close = body.find('>').ok_or_else(|| invalid(format!("cue {cue} has an invalid voice span")))?;
            let name = body[3..close].trim();
            if name.is_empty() || name.len() > 200 { return Err(invalid(format!("cue {cue} has an invalid speaker name"))); }
            speaker = Some(name.to_string());
            body = body[close+1..].trim_end_matches("</v>").to_string();
            if body.contains("<v ") || body.contains("</v>") { return Err(invalid(format!("cue {cue} has multiple voice spans; split it into separate cues"))); }
        }
        if body.trim().is_empty() { return Err(invalid(format!("cue {cue} has no text"))); }
        segments.push(NewSegment { start_ms,end_ms,speaker,text:body });
        if segments.len() > MAX_CUES { return Err(invalid("transcript contains too many cues")); }
    }
    if segments.is_empty() { return Err(invalid("transcript contains no cues")); }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn srt_preserves_raw_multiline_text_and_overlapping_speech() {
        let s = parse_subtitles("\u{feff}1\r\n00:00:01,250 --> 00:00:02,500\r\nAnn: um hello\r\nsecond line\r\n\r\n2\r\n00:00:02,000 --> 00:00:03,000\r\nBob: yes\r\n",SubtitleFormat::Srt).unwrap();
        assert_eq!((s[0].start_ms,s[0].end_ms),(1250,2500));
        assert_eq!(s[0].text,"Ann: um hello\nsecond line");
        assert!(s[0].speaker.is_none());
        assert_eq!(s.len(),2);
    }
    #[test]
    fn webvtt_voice_labels_and_cue_settings() {
        let s = parse_subtitles("WEBVTT\nKind: captions\n\nNOTE generated elsewhere\nignored\n\ncue-id\n01:02.003 --> 01:04.005 align:start\n<v Ann>um hello</v>\n",SubtitleFormat::Vtt).unwrap();
        assert_eq!(s[0].start_ms,62003);
        assert_eq!(s[0].speaker.as_deref(),Some("Ann"));
        assert_eq!(s[0].text,"um hello");
    }
    #[test]
    fn rejects_invalid_times_empty_text_and_multiple_voices() {
        for text in [
            "1\n00:00:01,000 --> 00:00:00,000\nbad",
            "1\n00:60:00,000 --> 00:61:00,000\nbad",
            "1\n00:00:00,00 --> 00:00:01,000\nbad",
            "1\n00:00:00,000 --> 00:00:01,000",
            "1\n99999999999999999:00:00,000 --> 99999999999999999:00:01,000\nbad",
        ] { assert!(parse_subtitles(text,SubtitleFormat::Srt).is_err(),"{text}"); }
        assert!(parse_subtitles("WEBVTT\n\n00:00.000 --> 00:01.000\n<v Ann>yes</v><v Bob>no</v>",SubtitleFormat::Vtt).is_err());
        assert!(parse_subtitles("not WebVTT",SubtitleFormat::Vtt).is_err());
    }
}
