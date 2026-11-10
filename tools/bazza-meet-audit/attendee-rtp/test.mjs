import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Script, createContext } from "node:vm";
import { instrumentRtp } from "./patch.mjs";

const [input] = process.argv.slice(2);
assert.ok(input, "Usage: node test.mjs <browser-audit.js>");
const browserAudit = readFileSync(input, "utf8");
const patched = instrumentRtp(browserAudit);
assert.equal(instrumentRtp(browserAudit.replace(/\r?\n/g, "\r\n")), patched);
assert.throws(() => instrumentRtp(patched), /Expected exact recorder payload/);
assert.match(patched, /const handleAudioTrack = async \(event\) => \{\n  bazzaMeetRtpAudit.register\(event.receiver\);/);
assert.match(patched, /setInterval\(\(\) => \{\n        void bazzaMeetRtpAudit.report\(ws\);/);
new Script(patched);

const start = patched.indexOf("const bazzaMeetRtpAudit =");
const end = patched.indexOf("const bazzaMeetAuditBrowser =", start);
let time = 1260;
const context = createContext({ performance: { timeOrigin: 1_790_000_000_000, now: () => time }, WebSocket: { OPEN: 1 } });
new Script(patched.slice(start, end) + "globalThis.audit = bazzaMeetRtpAudit;").runInContext(context);

const messages = [];
const client = { ws: { readyState: 0 }, sendJson: message => messages.push(JSON.parse(JSON.stringify(message))) };
const audio = (id, stats) => ({ id, type: "inbound-rtp", kind: "audio", ...stats });
const first = audio("fixture-first-id", {
    totalSamplesReceived: 48000,
    insertedSamplesForDeceleration: 40,
    removedSamplesForAcceleration: 16,
    concealedSamples: 12,
    jitterBufferEmittedCount: 47900,
    ssrc: 84956,
});
const second = audio("fixture-second-id", {
    totalSamplesReceived: 24000,
    insertedSamplesForDeceleration: 5,
    removedSamplesForAcceleration: -1,
    jitterBufferEmittedCount: 23900,
});
let finishPoll;
let polls = 0;
const receiver = {
    track: { readyState: "live" },
    getStats() {
        polls++;
        return new Promise(resolve => { finishPoll = resolve; });
    },
};
context.audit.register(receiver);
context.audit.register(receiver);
context.audit.register({ getStats() { throw new Error("fixture receiver failure"); } });
context.audit.register({ track: { readyState: "ended" }, getStats: () => assert.fail("ended receiver polled") });
context.audit.register({});

await context.audit.report(client);
assert.equal(polls, 0);
client.ws.readyState = 1;
const pending = context.audit.report(client);
await context.audit.report(client);
assert.equal(polls, 1);
time += 13;
finishPoll(new Map([
    [first.id, first],
    [second.id, second],
    ["fixture-video", { ...first, kind: "video" }],
    ["fixture-outbound", { ...first, type: "outbound-rtp" }],
]));
await pending;
assert.deepEqual(messages, [{
    type: "BazzaPlayoutAudit",
    stage: "inbound-rtp",
    browserEpochMs: 1_790_000_000_000,
    perfMs: 1260,
    pollDurationMs: 13,
    receivers: 2,
    receiverErrors: 1,
    inboundAudioReports: 2,
    totalSamplesReceived: 72000,
    totalSamplesReceivedReports: 2,
    insertedSamplesForDeceleration: 45,
    insertedSamplesForDecelerationReports: 2,
    removedSamplesForAcceleration: 16,
    removedSamplesForAccelerationReports: 1,
    concealedSamples: 12,
    concealedSamplesReports: 1,
    jitterBufferEmittedCount: 71800,
    jitterBufferEmittedCountReports: 2,
}]);
console.log("Offline fixture verified recorder overlay, numeric RTP totals, and private-field exclusion.");
