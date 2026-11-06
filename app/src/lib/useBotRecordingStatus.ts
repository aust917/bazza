import { useEffect, useState } from "react";
import { cmd, on, type AttendeeRecordingStatus, type DiscordRecordingStatus, type Events } from "./ipc";

type StatusEvent = "attendee_recording_status" | "discord_recording_status";

function useMeetingStatus<K extends StatusEvent>(
  meetingId: string | null,
  event: K,
  snapshot: () => Promise<Events[K] | null>,
  meetingOf: (status: Events[K]) => string,
) {
  const [status, setStatus] = useState<Events[K] | null>(null);
  useEffect(() => {
    if (!meetingId) return;
    let cancelled = false;
    let stop: (() => void) | undefined;
    let revision = 0;
    void on(event, (update) => {
      if (cancelled || meetingOf(update) !== meetingId) return;
      revision += 1;
      setStatus(update);
    }).then(async (unlisten) => {
      if (cancelled) return unlisten();
      stop = unlisten;
      const initialRevision = revision;
      const initial = await snapshot();
      if (!cancelled && initialRevision === revision && initial && meetingOf(initial) === meetingId) setStatus(initial);
    }).catch(() => {});
    return () => { cancelled = true; stop?.(); };
  }, [meetingId, event, snapshot, meetingOf]);
  return status && meetingOf(status) === meetingId ? status : null;
}

const attendeeMeeting = (status: AttendeeRecordingStatus) => status.meetingId;
const discordMeeting = (status: DiscordRecordingStatus) => status.meeting_id;

export function useBotRecordingStatus(meetingId: string | null) {
  const attendee = useMeetingStatus(meetingId, "attendee_recording_status", cmd.attendeeRecordingStatus, attendeeMeeting);
  const discord = useMeetingStatus(meetingId, "discord_recording_status", cmd.discordRecordingStatus, discordMeeting);
  return { attendeePhase: attendee?.phase ?? null, discordState: discord?.state ?? null };
}
