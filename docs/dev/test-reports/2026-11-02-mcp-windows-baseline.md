# MCP: Windows baseline после исправлений производительности

Дата: 2 ноября 2026 года, Asia/Yekaterinburg. Область — доступные без устройств,
пользовательской библиотеки и cloud-запросов проверки этапа 0
[плана MCP](../plans/2026-11-02-mcp-implementation.md).
Продолжает [первичный baseline](2026-11-02-mcp-baseline.md) и учитывает
[итог производительности](2026-11-02-performance-fixes.md).

Пройдены **22 native unit/integration tests** существующего Tauri release test EXE
и **3 проверки настоящего компонента Player в jsdom**. Микрофон/loopback/video
capture не запускались; production data-dir pointer не менялся. Новая сборка,
staging DLL/sidecars, GUI startup и inference в этой проверке не выполнялись.

## Происхождение native baseline

Использован `target/release/deps/bazza_lib-2a166daebae7e012.exe`, построенный
параллельной сессией производительности в 08:12:53. SHA-256:
`e9b5fbdeb056c3f8f3294d3b555f2004c7b68d0dc36f5646a2c4b071064bbed9`.
Его путь и исходная успешная сборка зафиксированы в
`.git/local-tests/20261102-performance-fixes/native-tests-final.log`.

Перед повтором в 08:29 проверены все **32 hashes** из performance
`source-manifest.json`: исходники и два release executables совпали. Все **108
проверенных source/template files** в локальных зависимостях Tauri имели timestamps
раньше link test EXE; самый поздний `src-tauri` source — `commands/meetings.rs`,
08:12:32. Hashes этих файлов и test EXE до/после повторов не изменились.
Evidence: `.git/local-tests/20261102-mcp/native-current-provenance.json`.

Это повтор существующего build artifact с проверенным performance source snapshot,
не новая герметичная сборка всего дерева. Последующие изменения Store для этапа 1
нужно проверять отдельно; этот baseline фиксирует состояние до них. Mtimes сами
по себе не заменяют build provenance: здесь они используются вместе с исходным
build/test log, именем artifact и сохранёнными source hashes.

## Выполненные native проверки

Воспроизводимый harness с проверками происхождения и полными logs хранится локально:

```powershell
& ./.git/local-tests/20261102-mcp/run-native-baseline.ps1
```

Он запускает этот test EXE с `--test-threads=1` и фильтрами ниже. Игнорируемый
FFmpeg test запускается отдельным exact-вызовом; `BAZZA_TEST_FFMPEG` указывает на
существующий `src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe` только в процессе
harness и затем восстанавливается. Все media/DB outputs создаются тестами в новых
temporary directories, без открытия default Bazza library.

| Фильтр / test | Результат | Подтверждённая граница |
| --- | --- | --- |
| `recorder::tests::` | 4 passed | Late writer failure не забирает новую запись; failed writer сохраняет synthetic WAV/raw video; успешное завершение выставляет processing; тяжёлая обработка ждёт startup/Stop lifecycle lock. |
| `settings::tests::` | 4 passed | Legacy defaults local STT, consent false, app audio scope, 15 FPS/1080p; settings roundtrip/validation и явный consent. |
| `stt_stage::tests::` | 3 passed | Permission/key до регистрации; revoke/provider change отменяют registered token; revoke/key clear не теряют регистрацию worker до ActiveRecording. Network worker не создаётся. |
| `processing_jobs::tests::` | 3 passed | FIFO/dedup до завершения job, один worker при concurrent submissions, deletion reservation исключает enqueue и освобождается при early exit. |
| `commands::meetings::correction_tests::` | 1 passed | Стабильные typed error codes и предел 65 536 UTF-8 bytes. |
| `import::tests::` без ignored | 3 passed | Название из filename, параметры 16 kHz/mono PCM, subtitles вне media duration отклоняются без clamping текста. |
| `video_diagnostic::tests::` | 3 passed | Diagnostic warning переживает reopen/optional failures; invalid/empty sidecar не стирает сохранённое предупреждение и не выходит из recording directory. |
| `import::tests::media_and_transcript_import_survives_library_reopen_without_inference`, `--exact --ignored` | 1 passed | Настоящий FFmpeg конвертирует synthetic 1 s WAV; VTT/raw/speaker/chunks переживают Store reopen; summary не создаётся, global voice identity не назначается. |

Итого **22 passed, 0 failed**. Import suite сначала сообщает один ignored test;
он затем отдельно выполнен успешно, поэтому незапущенного ignored случая в этом
выбранном наборе не осталось. Остальные Tauri tests не запускались повторно.
Команда не вызывает `start_recording`, `Recorder::start`, devices или Soniox.

## Player и существующий UI

Поскольку [Player](../../../app/src/components/Player.tsx) в Meeting tests подменён,
добавлен отдельный локальный harness без production-изменений:

```powershell
pnpm exec vitest run --config .git/local-tests/20261102-mcp/player-vitest.config.mts
```

**3 passed**, Vitest 5.0.0, 1,98 с; log — `player-ui-baseline.log` в том же
локальном каталоге. Проверены audio asset conversion, video preference, milliseconds
в time callback/slider, seek и play при paused, keyboard seek с нижней границей 0,
play/pause/ended state и освобождение media listeners. `HTMLMediaElement` play и
Tauri `convertFileSrc` подменены: WAV/MP4 decoder, настоящий asset protocol и A/V
synchronization этим не проверяются.

SHA-256 production Player при запуске совпадает с performance manifest:
`cbe34bc8966165c20866f9f17028856bfd66379d31c1b37a783f32377c7d66b3`.

Первичный baseline уже выполнил 41 UI test; повтор всего набора без новых UI-изменений
не нужен. Итоговый [performance report](2026-11-02-performance-fixes.md) отдельно
фиксирует 99 passing tests / 23 files, typecheck и production build. Сохраняются
его ограничения: полный DOM, отсутствие pagination, model/hardware coverage.

## Изменения source map после performance работы

До GUI cutover дополнить [writers audit](../research/2026-11-02-mcp-writer-audit.md)
следующими уточнениями:

- [processing_jobs](../../../src-tauri/src/processing_jobs.rs) теперь объединяет
  recovery, post-Stop/import и explicit reruns/deferred AI. Проверка lifecycle и
  `state.recording` задерживает новый heavy job; memory queue не является durable
  service jobs registry, lease или fencing protocol.
- [Store::update_processing_state](../../../crates/bazza-store/src/meetings.rs)
  публикует только processing-owned fields через UPDATE. Поздний snapshot не
  возвращает удалённую строку и не откатывает title/folder/participants. Существующий
  full-row `update_meeting`, включая rename path, по-прежнему требует замены
  targeted mutations/revisions при cutover.
- [Delete command](../../../src-tauri/src/commands/meetings.rs) резервирует ID в
  очереди, отклоняет active capture и защищает copy admission. Это улучшает текущий
  GUI lifecycle, но не заменяет service tombstone/generation/drain/purge и не
  разрешает MCP скрыто останавливать запись.
- [Store migrations](../../../crates/bazza-store/src/migrations.rs) теперь содержат
  append v9 indexes. Frozen synthetic fixture остаётся v8 и проверяет настоящий
  upgrade; service schema range нельзя считать равным 8.
- Async/off-thread commands, streaming WAV preparation и bounded ShareServer Stop
  уже входят в performance baseline. Публичные args/DTO и recording defaults не
  меняются из-за этого отчёта; resource ownership всё ещё принадлежит GUI.

## Состояние gates и отложенные проверки

| Gate | Состояние в доступном Windows software scope |
| --- | --- |
| Capture defaults, local/cloud policy, stop-result preservation, queue admission | Пройдено существующими native tests; настоящее открытие devices и полный pause/resume/stop waveform не запускались. |
| Correction/import/subtitles | Native validation и synthetic FFmpeg import/reopen пройдены; transactional Store/raw/manual/empty и bundle checks входят в отдельный согласованный Store/share набор. |
| Recovery | Failed-writer и diagnostic recovery/reopen пройдены. Performance runtime ранее восстановил 4 synthetic meetings через одну очередь с missing-ASR soft skip; свежий полный crash/restart capture run здесь не выполнялся. |
| Player | Пройден software state contract; настоящий media playback/seek и синхронизация отложены. |
| Sharing | Performance source snapshot/test report проверяет HTML/range/invalid token/disable и bounded Stop. Headless service sharing пока не реализован и не принимается этим baseline. |
| Summary-runner isolation | Первичный cached binary прошёл; current-source lean suite/новые MCP propagation checks должны указываться отдельно. Реальные Codex/Claude summary hosts здесь не запускались. |
| Synthetic v8 | Генератор и upgrade/reopen tests находятся в [test-support](../../../crates/bazza-service-test-support/README.md); собственные data/models не используются. |
| Detached lifecycle | Windows broker evidence хранится в `task-broker-evidence-reviewed.json`: temporary InteractiveToken/Limited task пережила kill launcher tree и удалена после проверки. Production bootstrap/IPC — отдельный этап. |
| macOS, hardware capture, real host compatibility | Отложено; Windows mocks или unit tests не закрывают эти platform/acceptance gates. |

Доступная Windows software baseline даёт основание разрабатывать service skeleton
на disposable fixtures. Она не разрешает production MCP writes до schema/GUI
cutover gates и не объявляет M1–M11 выполненными. После изменения capture/sync,
worker handoff или production Store ownership соответствующую границу нужно
проверить заново; текущие результаты не переносятся на новый lifecycle автоматически.
