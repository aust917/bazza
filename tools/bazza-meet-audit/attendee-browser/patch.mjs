import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export const revision = "eaacec16f2939b595f196b3957a717bbb0ed23e9";
const upstreamHash = "fc45d1876a94ee5d91ddaf87435ef31e42b2d687119edfe52db44760375dadb9";

const audit = `const bazzaMeetAuditBrowser = (() => {
    const startedAt = performance.now();
    const counters = {
        receiver_count: 0,
        csrc_snapshots: 0,
        roster_updates: 0,
        device_output_updates: 0,
        frames_seen: 0,
        frames_zero: 0,
        frames_nonzero: 0,
        frames_empty_sources: 0,
        frames_no_mapped_source: 0,
        frames_mapped: 0,
        frames_multiple_sources: 0,
        frames_multiple_mapped_sources: 0,
        frames_controller_inactive: 0,
        transform_errors: 0,
        send_attempts: 0,
        send_accepted: 0,
        send_socket_not_open: 0,
        send_media_disabled: 0,
        send_errors: 0,
    };
    function report(client) {
        if (client.ws.readyState === WebSocket.OPEN) {
            client.sendJson({
                type: 'BazzaMeetAuditBrowser',
                schema: 1,
                elapsed_ms: Math.round(performance.now() - startedAt),
                counters: { ...counters }
            });
        }
    }
    return { counters, report };
})();

`;

function replaceOnce(source, before, after) {
    const offset = source.indexOf(before);
    if (offset < 0 || source.indexOf(before, offset + before.length) !== -1) {
        throw new Error(`Unexpected upstream anchor: ${before.split("\n")[0]}`);
    }
    return source.slice(0, offset) + after + source.slice(offset + before.length);
}

function insertAfter(source, anchor, text) {
    return replaceOnce(source, anchor, anchor + "\n" + text);
}

function instrumentSender(sender) {
    sender = insertAfter(sender, "  sendPerParticipantAudio(participantId, audioData) {",
        "    bazzaMeetAuditBrowser.counters.send_attempts++;");
    sender = insertAfter(sender, "    if (this.ws.readyState !== WebSocket.OPEN) {",
        "      bazzaMeetAuditBrowser.counters.send_socket_not_open++;");
    sender = insertAfter(sender, "    if (!this.mediaSendingEnabled) {",
        "      bazzaMeetAuditBrowser.counters.send_media_disabled++;");
    sender = insertAfter(sender, "        this.ws.send(message.buffer);",
        "        bazzaMeetAuditBrowser.counters.send_accepted++;");
    return insertAfter(sender, "    } catch (error) {",
        "        bazzaMeetAuditBrowser.counters.send_errors++;");
}

function instrumentAudioTrack(track) {
    track = insertAfter(track, "const handleAudioTrack = async (event) => {",
        "  const counters = bazzaMeetAuditBrowser.counters;\n  counters.receiver_count++;");
    track = insertAfter(track, "            if (!frame) {\n                return;\n            }",
        "            counters.frames_seen++;");
    track = insertAfter(track, "                if (controller.desiredSize === null) {",
        "                    counters.frames_controller_inactive++;");
    track = insertAfter(track, "                if (audioData.every(value => value === 0)) {",
        "                    counters.frames_zero++;");
    const sourceLookup = "                const contributingSources = receiverManager.getContributingSources(receiver);";
    track = replaceOnce(track, sourceLookup, `                counters.frames_nonzero++;
${sourceLookup}
                if (contributingSources.length === 0) counters.frames_empty_sources++;
                if (contributingSources.length > 1) counters.frames_multiple_sources++;`);
    track = insertAfter(track,
        "                const userForContributingSourceWithLoudestAudio = usersForContributingSourcesWithAudioLevel[0]?.user;",
        `                if (usersForContributingSourcesWithAudioLevel.length > 1) counters.frames_multiple_mapped_sources++;
                if (userForContributingSourceWithLoudestAudio?.deviceId) {
                    counters.frames_mapped++;
                } else if (contributingSources.length > 0) {
                    counters.frames_no_mapped_source++;
                }`);
    return insertAfter(track, "            } catch (error) {\n                console.error('Error processing frame:', error);",
        "                counters.transform_errors++;");
}

function patchSection(source, start, end, patch) {
    const first = source.indexOf(start);
    const last = source.indexOf(end, first + start.length);
    if (first < 0 || last < 0) {
        throw new Error(`Missing upstream section: ${start}`);
    }
    const section = source.slice(first, last);
    return replaceOnce(source, section, patch(section));
}

export function instrumentBrowser(original) {
    let source = original.replaceAll("\r\n", "\n");
    const hash = createHash("sha256").update(source).digest("hex");
    if (hash !== upstreamHash) {
        throw new Error(`Expected unmodified Attendee ${revision} browser payload (SHA-256 ${upstreamHash}); got ${hash}`);
    }

    source = replaceOnce(source, "class ReceiverManager {", audit + "class ReceiverManager {");
    source = insertAfter(source, "    updateContributingSources(receiver, result) {",
        "        bazzaMeetAuditBrowser.counters.csrc_snapshots++;");
    source = insertAfter(source, "    updateDeviceOutputs(deviceOutputs) {",
        "        bazzaMeetAuditBrowser.counters.device_output_updates++;");
    source = insertAfter(source, "    newUsersListSynced(newUsersListRaw) {",
        "        bazzaMeetAuditBrowser.counters.roster_updates++;");
    source = insertAfter(source, "const ws = new WebSocketClient();\nwindow.ws = ws;",
        `if (window.initialData.sendPerParticipantAudio) {
    setInterval(() => bazzaMeetAuditBrowser.report(ws), 1000);
}`);
    source = patchSection(source, "  async disableMediaSending() {", "  handleMessage(data) {", disable =>
        insertAfter(disable, "    this.mediaSendingEnabled = false;",
            "    if (window.initialData.sendPerParticipantAudio) bazzaMeetAuditBrowser.report(this);"));
    source = patchSection(source, "  sendPerParticipantAudio(", "  sendPerParticipantVideo(", instrumentSender);
    return patchSection(source, "const handleAudioTrack = async (event) => {", "new RTCInterceptor({", instrumentAudioTrack);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
    try {
        const [input, output, extra] = process.argv.slice(2);
        if (!input || !output || extra) throw new Error("Usage: node patch.mjs <original.js> <new-output.js>");
        const patched = instrumentBrowser(readFileSync(input, "utf8"));
        writeFileSync(output, patched, { flag: "wx" });
    } catch (error) {
        console.error(error.message);
        process.exitCode = 1;
    }
}
