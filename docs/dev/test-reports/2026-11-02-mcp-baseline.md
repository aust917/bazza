# MCP: исходная проверка поведения перед service cutover

Дата: 2 ноября 2026 года. Среда: Windows, локальное рабочее дерево.
Область: этап 0 [плана MCP](../plans/2026-11-02-mcp-implementation.md).

Это baseline существующих проверяемых границ, а не приёмка headless MCP или
доказательство сохранения записи после будущего GUI cutover. Пользовательская
библиотека, устройства захвата, реальные встречи, модели и Soniox не использовались.
Параллельная сессия исправляет производительность; native сборка и staging в этом
baseline не запускались, production-код не менялся.

## Выполненные проверки

Из корня репозитория:

```powershell
pnpm exec vitest run src/windows/Popup.test.tsx src/windows/RecordBar.test.tsx src/routes/Settings.test.tsx src/components/Transcript.test.tsx src/routes/Meeting.test.tsx src/lib/export.test.ts
```

Результат: **6 файлов, 41 тест, 0 ошибок**, Vitest 5.0.0, 2,75 с.
Проверка выполнена 2 ноября в 08:00 по Asia/Yekaterinburg на текущих исходниках.
Mocks заменяют native IPC; это не запуск записи или воспроизведения в Tauri.

| Граница | Что фактически проверено |
| --- | --- |
| Picker и начало записи | Явный Record/Enter передаёт выбранный audio PID/scope; audio-only, mic-only и monitor fallback сохраняются. Detection/смена языка сами не начинают capture. macOS проверяется только подставленным user-agent. |
| RecordBar | Переход в paused, Resume/Stop labels, скрытие предыдущей caption в паузе, сохранение предупреждений до idle. Нажатия native Pause/Stop и результат WAV здесь не проверяются. |
| Настройки | Существующие 15 FPS/1080p, сохранение для следующей записи, откат ошибки; Soniox недоступен без явного consent, private key сохраняется отдельной командой. |
| Raw/clean/correction | Raw остаётся доступным, manual/empty/reset semantics, отсутствие случайного seek при редактировании; поздние responses другой встречи и старые reads не затирают принятую правку. |
| Summary | Correction не запускает генерацию; active job и сохранённый summary не протекают в другой route; retranscribe предупреждает о manual overlays. |
| Export/subtitles | TXT/MD/SRT, timestamps, отсутствие zero-length cue; намеренно пустые corrections исключаются из corrected export, raw сохраняется. |
| Playback | Подсветка по времени и seek callback транскрипта. Настоящий HTML media decoder, asset protocol и seek по WAV/MP4 не запускались. |

Для summary-runner выполнен уже существовавший test EXE без Cargo/build:

```powershell
& ./target/release/deps/bazza_summary_runner-79ec257f54328ddf.exe --test-threads=1
```

Результат: **3 passed**, 0 failed, 0,60 с. Fake CLI — этот же test binary;
настоящие Codex/Claude, подписка и сеть не используются. Проверены запрет tools и
transcript в arguments, UTF-8 stdin roundtrip, exit 7, timeout, output bound,
cancellation и завершение descendant, удерживающего pipes.

SHA-256 EXE: `B69868A47B353D5B42FF1729B1A418137D0D2BF4C74F75A15951CB37AA7D3873`.
SHA-256 [исходника runner](../../../crates/bazza-summary-runner/src/lib.rs)
при проверке: `213C243AD2FB1AD7D80ABD2692A8118CD5C1139FAEBCEB2F2662FA3EAE4B91D9`.
Build manifest, связывающего эти исходники с EXE, нет: это **проверка cached binary**,
не доказательство текущей source build. Текущий runner следует включить в следующий
согласованный `cargo test -p bazza-summary-runner`.

## Зафиксированное поведение записи по исходникам

[Settings::default](../../../src-tauri/src/settings.rs) сохраняет local STT,
`soniox_consent=false`, mic/output default device, `capture_system=true`,
`audio_scope=app`, video enabled, 15 FPS/1080p, `close_to_tray=true`.
Это source inspection; native settings tests в этой проверке не выполнялись.

[Start/Pause/Resume/Stop commands](../../../src-tauri/src/commands/recording.rs)
выполняют blocking lifecycle вне главного потока. В
[recorder](../../../src-tauri/src/recorder.rs) start сериализован lifecycle lock,
проверяет active recording и data-copy lock, сохраняет выбор Soniox до открытия
устройств. Local writer независим от выбранного STT. Pause/Resume публикуют реальное
состояние даже при ошибке записи video pause metadata.

Stop завершает audio/video, forwarder, STT и captions, сохраняет meeting и highlights,
публикует idle и ставит постобработку в существующую очередь. Ошибка writer сохраняет
доступные файлы и выставляет failed; поздняя ошибка старого writer не забирает новую
запись. Recovery помечает missing/слишком короткое audio как failed, а пригодное
interrupted audio передаёт в постобработку. При cutover это необходимо проверить
повторно, включая durable handoff и selected-provider provenance.

## Native coverage, которую нужно выполнить до cutover

Следующие тесты найдены в исходниках, но **не запускались** этим baseline:

| Область | Существующая полезная проверка / незакрытая граница |
| --- | --- |
| Consent revoke | [stt_stage tests](../../../src-tauri/src/stt_stage.rs): current permission/key before register; смена на local/revoke отменяет worker; revoke/key clear не пропускает registration до ActiveRecording. Реальный upload cancellation и native recording не проверены. |
| Writer/recovery | [recorder tests](../../../src-tauri/src/recorder.rs): `old_writer_failure_cannot_take_a_new_recording`, `failed_writer_keeps_partial_inputs_and_marks_failed`, `successful_capture_keeps_the_processing_path`. Полные start/pause/resume/stop, crash/restart и orphan recovery требуют отдельного synthetic native run. |
| Persisted corrections | [segments tests](../../../crates/bazza-store/src/segments.rs): raw/provenance/search, empty/whitespace/UTF-8, cleanup/speaker rename. Нужна согласованная проверка актуальных исходников Store. |
| Import | [Store import](../../../crates/bazza-store/src/import.rs): DB rollback/retry и orphan preservation; [bundle](../../../crates/bazza-share/src/bundle.rs): roundtrip/duplicates, invalid overlay/path/media, interrupted import; [subtitles](../../../crates/bazza-share/src/subtitles.rs): SRT/VTT и invalid timestamps. |
| Sharing | [server tests](../../../crates/bazza-share/tests/server.rs): HTML escaping, range bytes, invalid token, disabled share 404; bounded Stop при unread response. Headless keepalive ещё не реализован. |
| Player | Отдельного Player test нет; Meeting suite подменяет Player. Требуются asset-protocol playback/seek и A/V на synthetic media. |
| Summary isolation | Cached EXE проверен; текущая source build и реальные host versions отдельно. |
| macOS | Нативная проверка на macOS не выполнялась. Подмена user-agent не заменяет ScreenCaptureKit, UDS, process groups или native acceptance. |

## Критерий следующего шага

UI baseline проходит. Этап 0 целиком не закрыт этим отчётом: нужны synthetic v8
fixture, согласованные Store/share/runner tests, detached lifecycle spike из реального
host и native capture/recovery acceptance. Ни одна пустая checkbox плана не становится
выполненной только по наличию теста в исходниках или по историческому отчёту.

До service cutover MCP mutations рабочей библиотеки остаются выключенными.
Текущая проверка не меняет capture defaults, DB schema, credentials или GUI lifecycle.
