import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const pinnedHash = "f7226ab9a5d355f09ec67ed62a9c8a0412cddfc99431035871ba020e8abcaffa";

function replaceOnce(source, anchor, replacement) {
    const first = source.indexOf(anchor);
    if (first < 0 || source.indexOf(anchor, first + anchor.length) >= 0) {
        throw new Error("Pinned playout anchor missing or ambiguous");
    }
    return source.slice(0, first) + replacement + source.slice(first + anchor.length);
}

export function patch(source) {
    if (createHash("sha256").update(source).digest("hex") !== pinnedHash) {
        throw new Error("Shared Chrome payload differs from the pinned Attendee snapshot");
    }

    const method = "    async playPCMAudio(pcmData, sampleRate = 44100, numChannels = 1) {";
    source = replaceOnce(source, method, `    _bazzaPlayoutChunk() {
        if (!window.googleMeetInitialData || !window.ws || (this.bazzaPlayoutSequence ?? 0) >= 128) {
            return 0;
        }
        this.bazzaPlayoutSequence = (this.bazzaPlayoutSequence ?? 0) + 1;
        return this.bazzaPlayoutSequence;
    }

    _bazzaPlayoutAudit(stage, sequence, details) {
        if (!sequence) return;
        try {
            const context = this.audioContext;
            const output = context.getOutputTimestamp?.();
            const fields = {
                type: "BazzaPlayoutAudit",
                stage,
                sequence,
                browserEpochMs: performance.timeOrigin,
                perfMs: performance.now(),
                contextMs: context.currentTime * 1000,
                contextRateHz: context.sampleRate,
                contextState: ["suspended", "running", "closed", "interrupted"].indexOf(context.state),
                outputContextMs: output ? output.contextTime * 1000 : null,
                outputPerfMs: output?.performanceTime ?? null,
                baseLatencyMs: Number.isFinite(context.baseLatency) ? context.baseLatency * 1000 : null,
                outputLatencyMs: Number.isFinite(context.outputLatency) ? context.outputLatency * 1000 : null,
                ...details,
            };
            queueMicrotask(() => {
                try { window.ws?.sendJson(fields); } catch (_) { /* diagnostics are optional */ }
            });
        } catch (_) { /* diagnostics are optional */ }
    }

${method}`);

    const enqueue = `        this.audioQueue.push({
            data: audioData,
            duration,
        });`;
    source = replaceOnce(source, enqueue, `        const auditSequence = this._bazzaPlayoutChunk();
        this.audioQueue.push({
            data: audioData,
            duration,
            auditSequence,
        });
        this._bazzaPlayoutAudit("queued", auditSequence, {
            inputSamples: audioData.length,
            inputRateHz: sampleRate,
            inputChannels: numChannels,
            bufferMs: duration * 1000,
            queueDepth: this.audioQueue.length,
        });`);

    const current = "        const currentTime = this.audioContext.currentTime;";
    source = replaceOnce(source, current, `${current}
        const priorPlayTime = this.nextPlayTime;`);
    source = replaceOnce(source, "        const { data, duration } = this.audioQueue.shift();",
        "        const { data, duration, auditSequence } = this.audioQueue.shift();");

    const schedule = `        source.start(this.nextPlayTime);
        this.nextPlayTime += duration;`;
    return replaceOnce(source, schedule, `        const scheduledAt = this.nextPlayTime;
${schedule}
        if (auditSequence) {
            try {
                const scheduled = {
                    schedulerContextMs: currentTime * 1000,
                    priorEndMs: priorPlayTime ? priorPlayTime * 1000 : null,
                    lateResetMs: priorPlayTime ? Math.max(0, currentTime - priorPlayTime) * 1000 : 0,
                    startMs: scheduledAt * 1000,
                    endMs: this.nextPlayTime * 1000,
                    bufferMs: audioBuffer.duration * 1000,
                    frames: audioBuffer.length,
                    rateHz: audioBuffer.sampleRate,
                    playbackRate: source.playbackRate.value,
                    queueRemaining: this.audioQueue.length,
                };
                this._bazzaPlayoutAudit("scheduled", auditSequence, scheduled);
                source.addEventListener("ended", () => this._bazzaPlayoutAudit("ended", auditSequence, {
                    startMs: scheduled.startMs,
                    endMs: scheduled.endMs,
                }), { once: true });
            } catch (_) { /* diagnostics are optional */ }
        }`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
    const [, , input, output] = process.argv;
    if (!input || !output || process.argv.length !== 4) {
        throw new Error("Usage: node patch.mjs <pinned shared Chrome payload> <new output file>");
    }
    writeFileSync(output, patch(readFileSync(input, "utf8")), { flag: "wx" });
}
