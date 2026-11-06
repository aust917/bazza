import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { serviceFixture } from "./service-fixture-ipc";

const native = vi.hoisted(() => ({ invoke: vi.fn().mockResolvedValue(null) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
beforeEach(() => { native.invoke.mockClear(); Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} }); });
afterEach(() => { Reflect.deleteProperty(window, "__TAURI_INTERNALS__"); });

it("wraps each request exactly once and uses only the fixture command namespace", async () => {
  await serviceFixture.bootstrap(); await serviceFixture.preferences(); await serviceFixture.reconnect();
  await serviceFixture.meeting("fixture-id");
  const request = { expected_revision: "revision-1", idempotency_key: "key-1", patch: { ui_locale: "ru" as const } };
  await serviceFixture.updatePreferences(request);
  await serviceFixture.captureStatus(); await serviceFixture.captureStart(); await serviceFixture.capturePause(); await serviceFixture.captureResume(); await serviceFixture.captureStop();
  expect(native.invoke.mock.calls).toEqual([
    ["service_fixture_bootstrap", undefined], ["service_fixture_preferences", undefined], ["service_fixture_reconnect", undefined],
    ["service_fixture_meeting", { request: { meeting_id: "fixture-id" } }], ["service_fixture_update_preferences", { request }],
    ["service_fixture_capture_status", undefined], ["service_fixture_capture_start", { request: { title: null, capture_screen: false, use_configured_audio: false } }],
    ["service_fixture_capture_pause", undefined], ["service_fixture_capture_resume", undefined], ["service_fixture_capture_stop", undefined],
  ]);
});

it("fails explicitly outside Tauri without invoking a mock or native command", async () => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  await expect(serviceFixture.bootstrap()).rejects.toMatchObject({ code: "SERVICE_UNAVAILABLE", retryable: false });
  expect(native.invoke).not.toHaveBeenCalled();
});
