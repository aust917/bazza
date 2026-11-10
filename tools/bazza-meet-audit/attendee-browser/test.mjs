import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Script, createContext } from "node:vm";
import { instrumentBrowser } from "./patch.mjs";

const [input] = process.argv.slice(2);
assert.ok(input, "Usage: node test.mjs <original-pinned.js>");
const original = readFileSync(input, "utf8");
const patched = instrumentBrowser(original);
assert.equal(instrumentBrowser(original.replace(/\r?\n/g, "\r\n")), patched);
assert.throws(() => instrumentBrowser(original + "\n"), /Expected unmodified Attendee/);
assert.throws(() => instrumentBrowser(patched), /Expected unmodified Attendee/);
new Script(patched);

function section(start, end) {
    const first = patched.indexOf(start);
    const last = patched.indexOf(end, first + start.length);
    assert.ok(first >= 0 && last > first);
    return patched.slice(first, last);
}

let time = 200;
let sources = [];
let transform;
let sent = 0;
let enqueued = 0;
let closed = 0;
const reports = [];
const clientSocket = { readyState: 0, send() { sent++; } };
const context = createContext({
    performance: { now: () => time },
    WebSocket: { OPEN: 1 },
    TextEncoder,
    console: { log() {}, error() {} },
    clientSocket,
    reports,
    receiverManager: { getContributingSources: () => sources },
    userManager: {
        getUserByStreamId: id => ["1", "2"].includes(id) ? { deviceId: `fixture-${id}` } : null,
    },
    MediaStreamTrackProcessor: class {
        readable = { pipeThrough: () => ({ pipeTo: async () => {} }) };
    },
    MediaStreamTrackGenerator: class {},
    TransformStream: class {
        constructor(callbacks) { transform = callbacks.transform; }
    },
});

const audit = section("const bazzaMeetAuditBrowser =", "class ReceiverManager {");
const audio = section("const handleAudioTrack = async (event) => {", "new RTCInterceptor({");
const sender = section("  sendPerParticipantAudio(", "  sendPerParticipantVideo(");
new Script(`${audit}
class WebSocketClient {
    static MESSAGE_TYPES = { PER_PARTICIPANT_AUDIO: 5 };
    ws = clientSocket;
    mediaSendingEnabled = false;
    sendJson(message) {
        if (message.type === 'BazzaMeetAuditBrowser') reports.push(message);
    }
${sender}
}
const ws = new WebSocketClient();
${audio}
globalThis.capture = handleAudioTrack;
globalThis.client = ws;
globalThis.report = () => bazzaMeetAuditBrowser.report(ws);
`).runInContext(context);
await context.capture({ track: {}, streams: [], receiver: {} });

const controller = { desiredSize: 1, enqueue() { enqueued++; } };
const source = (id, level = 1) => ({ source: id, audioLevel: level });
const frame = (value, copyThrows = false) => ({
    numberOfChannels: 1,
    numberOfFrames: 1,
    sampleRate: 48000,
    format: "f32",
    duration: 20,
    copyTo(target) {
        if (copyThrows) throw new Error("fixture read failed");
        target[0] = value;
    },
    close() { closed++; },
});

context.report();
assert.equal(reports.length, 0);
await transform(frame(0), controller);
await transform(frame(1), controller);
sources = [source(99)];
await transform(frame(1), controller);
sources = [source(99, 0.1), source(1)];
await transform(frame(1), controller);
clientSocket.readyState = 1;
sources = [source(1, 0.5), source(2)];
await transform(frame(1), controller);
context.client.mediaSendingEnabled = true;
sources = [source(1)];
const send = clientSocket.send;
clientSocket.send = () => { throw new Error("fixture send failed"); };
await transform(frame(1), controller);
clientSocket.send = send;
await transform(frame(1), controller);
await transform(frame(1), { ...controller, desiredSize: null });
await transform(frame(1, true), controller);

time = 1530;
context.report();
assert.equal(sent, 1);
assert.equal(enqueued, 6);
assert.equal(closed, 2);
assert.equal(reports.length, 1);
assert.deepEqual(JSON.parse(JSON.stringify(reports[0])), {
    type: "BazzaMeetAuditBrowser",
    schema: 1,
    elapsed_ms: 1330,
    counters: {
        receiver_count: 1,
        csrc_snapshots: 0,
        roster_updates: 0,
        device_output_updates: 0,
        frames_seen: 9,
        frames_zero: 1,
        frames_nonzero: 6,
        frames_empty_sources: 1,
        frames_no_mapped_source: 1,
        frames_mapped: 4,
        frames_multiple_sources: 2,
        frames_multiple_mapped_sources: 1,
        frames_controller_inactive: 1,
        transform_errors: 1,
        send_attempts: 4,
        send_accepted: 1,
        send_socket_not_open: 1,
        send_media_disabled: 1,
        send_errors: 1,
    },
});
console.log("Pinned patch, generated JavaScript syntax, and browser diagnostic counters verified offline.");
