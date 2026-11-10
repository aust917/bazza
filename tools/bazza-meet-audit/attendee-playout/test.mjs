import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { patch } from "./patch.mjs";

const source = readFileSync(process.argv[2], "utf8");
assert.throws(() => patch(source + "\n"), /pinned/);
const timers = [];
const microtasks = [];
const events = [];
const started = [];
const sources = [];
const audioContext = {
    currentTime: 0,
    sampleRate: 48000,
    state: "running",
    baseLatency: 0.010,
    getOutputTimestamp: () => ({ contextTime: 0.5, performanceTime: 501 }),
    createBuffer(channels, length, sampleRate) {
        return { channels, length, sampleRate, duration: length / sampleRate,
            getChannelData: () => ({ set() {} }) };
    },
    createBufferSource() {
        const source = {
            connect() {},
            start(time) { started.push(time); },
            playbackRate: { value: 1 },
            addEventListener(event, callback, options) {
                assert.equal(event, "ended");
                assert.equal(options.once, true);
                this.end = callback;
            },
        };
        sources.push(source);
        return source;
    },
};

let perf = 0;
const window = { googleMeetInitialData: {}, ws: { sendJson: event => events.push(event) } };
const context = vm.createContext({ window, performance: { timeOrigin: 1000, now: () => perf },
    setTimeout: callback => timers.push(callback), queueMicrotask: callback => microtasks.push(callback) });
const patched = patch(source);
const BotOutputManager = new vm.Script(`${patched}\nBotOutputManager`).runInContext(context);
const sender = Object.create(BotOutputManager.prototype);
Object.assign(sender, { audioContext, audioQueue: [], sampleRate: 44100, numChannels: 1,
    _createSourceAudioTrack() {}, ensureMicOn() {} });

await sender.playPCMAudio(Array(44100).fill(123));
await sender.playPCMAudio(Array(22050).fill(456));
perf = 1110;
audioContext.currentTime = 1.11;
timers.shift()();
sources[0].end();
while (microtasks.length) microtasks.shift()();

assert.deepEqual(started, [0, 1.11]);
assert.deepEqual(events.map(({ stage, sequence }) => [stage, sequence]),
    [["queued", 1], ["scheduled", 1], ["queued", 2], ["scheduled", 2], ["ended", 1]]);
const scheduled = events[3];
assert.equal(scheduled.browserEpochMs, 1000);
assert.equal(scheduled.perfMs, 1110);
assert.equal(scheduled.contextRateHz, 48000);
assert.equal(scheduled.rateHz, 44100);
assert.equal(scheduled.bufferMs, 500);
assert.equal(scheduled.frames, 22050);
assert.equal(scheduled.playbackRate, 1);
assert.equal(scheduled.priorEndMs, 1000);
assert.ok(Math.abs(scheduled.lateResetMs - 110) < 1e-6);
for (const event of events) {
    for (const [key, value] of Object.entries(event)) {
        if (key !== "type" && key !== "stage") assert.ok(value === null || Number.isFinite(value));
    }
}

delete window.googleMeetInitialData;
await sender.playPCMAudio([123]);
assert.equal(microtasks.length, 0);
window.googleMeetInitialData = {};
sender.bazzaPlayoutSequence = 128;
await sender.playPCMAudio([123]);
assert.equal(microtasks.length, 0);

console.log("PASS pinned script parses, 110 ms late reset measured, clocks/rates reported, no payload logged");
