# MCP: аудит владельцев данных и lifecycle

Дата: 2 ноября 2026 года. Часть [этапа 0 MCP](../plans/2026-11-02-mcp-implementation.md).
Это инвентаризация исходников, а не подтверждение native acceptance. Пользовательская
библиотека не открывалась; существующий код не менялся. Во время аудита параллельная
сессия производительности обновляет перечисленные ниже файлы, поэтому перед cutover
инвентаризацию нужно повторить по итоговому дереву. Таблицы ниже сохраняют исходный
snapshot; изменения service foundation перечислены в завершающем разделе этапа 1.

## Открытие SQLite и неявные writes: исходный snapshot

| Участок | Поведение исходного snapshot | Требование к cutover |
| --- | --- | --- |
| [lib.rs](../../../src-tauri/src/lib.rs), `run` | Единственный production `Store::open`; выполняется до создания Tauri Builder и регистрации single-instance plugin. Затем `restore_saved` может менять DB | Service ownership и schema handshake должны предшествовать migrations/recovery. Tauri single-instance сам по себе не защищает от второго opener |
| [state.rs](../../../src-tauri/src/state.rs), `AppState` / `new` | Владеет `Arc<Store>`; читает settings/locale, seed templates, хранит GUI engines/capture/share/calendar state | Заменить DB ownership на client; оставить GUI engines и native capture. Seed больше не должен требовать `bazza_llm` |
| [db.rs](../../../crates/bazza-store/src/db.rs), `Store::open` / `init` | Создаёт parent directory и DB, включает WAL/FK, запускает migrations при каждом open | Разделить open-existing/create/migrate; удерживать canonical-library guard весь lifetime. Неверный путь не должен создавать библиотеку |
| [migrations.rs](../../../crates/bazza-store/src/migrations.rs), `run` | Читает user_version, применяет каждую миграцию своей transaction; future user_version сейчас не отклоняет | Проверять supported schema до PRAGMA/recovery/других writes. Сохранить upgrade с v8; performance-сессия уже добавила v9 indexes |
| [db.rs](../../../crates/bazza-store/src/db.rs), `data_dir` / `models_dir` / `recordings_dir` | Создают директории даже при вызове из пути чтения | DTO/чтение и path lookup должны обходиться без mkdir; отдельное разрешение service/worker на создание |
| [db.rs](../../../crates/bazza-store/src/db.rs), `checkpoint` / `snapshot_to` | Checkpoint меняет WAL; snapshot резервирует новый файл и выполняет `VACUUM INTO`, sync, cleanup при ошибке | Service-owned snapshot, с учётом leases/readers и relocation epoch |
| [db.rs](../../../crates/bazza-store/src/db.rs), `write_data_dir_pointer` | Создаёт/удаляет `data-dir.txt` в default directory | Только relocation protocol сервиса |
| [share/server.rs](../../../crates/bazza-share/src/server.rs) | Хранит ещё один `Arc<Store>`; HTTP читает DB и media, listener принадлежит GUI | Перенести lifetime listener в service; read streams требуют lifecycle coordination |

`Store::conn` доступен только внутри bazza-store (`pub(crate)`). Production
`rusqlite::Connection::open` и SQL transactions находятся в этом crate. Остальные
найденные `Store::open` / `open_in_memory` относятся к `#[cfg(test)]`, examples и
integration tests; при zero-write gate их следует отличать от production startup.
Новые MCP packages, создаваемые параллельно этому аудиту, в эту исходную карту не входят.

## Репозитории, способные менять DB

Список методов включает косвенные изменения FTS/chunks/vectors, а не только имена
`insert`/`update`. Источник истины для SQL — соответствующий файл репозитория.

| Модуль bazza-store | Persistence entry points |
| --- | --- |
| [meetings.rs](../../../crates/bazza-store/src/meetings.rs) | `create_meeting`, `update_meeting` (full row), `set_meeting_error`, `set_participants`, `set_title`, `delete_meeting` |
| [segments.rs](../../../crates/bazza-store/src/segments.rs) | `replace_segments`, `set_segment_speakers`, `set_transcript_correction`; correction сохраняет raw, manual/empty/reset и перестраивает retrieval |
| [speakers.rs](../../../crates/bazza-store/src/speakers.rs) | `upsert_speaker`, `delete_speaker`, `set_meeting_speakers`, `rename_speaker`, `set_clean_text`, `clear_clean_text`; pending suggestions и retrieval тоже инвалидируются |
| [voice_samples.rs](../../../crates/bazza-store/src/voice_samples.rs) | `create_voice_profile`, `rename_voice_profile`, `add_voice_sample`, `delete_voice_sample`, `recompute_voice_profile`; профиль и compatibility могут меняться вместе с sample |
| [organize.rs](../../../crates/bazza-store/src/organize.rs) | `create_folder`, `update_folder`, `delete_folder`, `move_meeting`, `tag_meeting`, `untag_meeting`, `create_share`, `set_share_enabled`, `delete_share` |
| [ai.rs](../../../crates/bazza-store/src/ai.rs) | `upsert_template`, `seed_templates`, `delete_template`, `set_summary`, `create_chat`, `rename_chat`, `delete_chat`, `add_message`, `update_message`, `rebuild_chunks` |
| [vectors.rs](../../../crates/bazza-store/src/vectors.rs) | `set_chunk_vectors`, `set_chunk_vectors_if_current` |
| [settings.rs](../../../crates/bazza-store/src/settings.rs) | Generic `set_setting`; ключи ниже являются разными доменными/секретными контрактами, а не публичным generic MCP method |
| [import.rs](../../../crates/bazza-store/src/import.rs) | `import_meeting`: transaction metadata/segments/speakers/summary/tags/highlights/STT/chunks, установка managed media и recovery marker |

В текущих writers нет общего expected_revision/idempotency/change-sequence
протокола. Удаление и последующий поздний worker необходимо проверять по generation,
а не только по успешности SQL. Текущий `delete_meeting` чистит scoped `stt` и
`stt_previous`; это не заменяет полный purge из плана MCP.

## Production callers DB mutations

| Участок Tauri | Writes / дополнительные producers |
| --- | --- |
| [commands/meetings.rs](../../../src-tauri/src/commands/meetings.rs) | Transcript correction; rename через full-row `update_meeting`; delete DB + files + cancellation summary; reruns/import направляются в recorder/pipeline/import |
| [commands/organize.rs](../../../src-tauri/src/commands/organize.rs) | Folder/tag/share CRUD; share preparation; bundle import; snapshot/copy/pointer change |
| [commands/ai.rs](../../../src-tauri/src/commands/ai.rs) | Template/chat CRUD; user message, placeholder assistant и итог stream message; summary через summary_stage |
| [commands/speakers.rs](../../../src-tauri/src/commands/speakers.rs) | Explicit rename/accept suggestion, legacy speaker deletion, voice profile/sample CRUD; embeddings-map remap; delayed cleanup/reembed/summary refresh |
| [commands/settings.rs](../../../src-tauri/src/commands/settings.rs), [settings.rs](../../../src-tauri/src/settings.rs), [ui_locale.rs](../../../src-tauri/src/ui_locale.rs) | Settings и locale persistence; provider/consent change coordinated с STT cancellation |
| [commands/recording.rs](../../../src-tauri/src/commands/recording.rs) | `dismiss_detection(never)` сохраняет never_apps; start/stop/pause/resume вызывают recorder |
| [recorder.rs](../../../src-tauri/src/recorder.rs) | Capture create/finalize/failure, orphan recovery, participant sampler, highlights, retranscribe status; STT selection сохраняется до device/network startup |
| [pipeline.rs](../../../src-tauri/src/pipeline.rs) | Segments/provenance/speakers/embeddings, meeting status/errors, cleanup overlays, chunks; deferred AI/embedding и explicit reruns тоже writers |
| [embed_stage.rs](../../../src-tauri/src/embed_stage.rs) | Publish chunk vectors; conditional current-text check уже защищает часть stale commits, но не заменяет lifecycle fencing |
| [summary_stage.rs](../../../src-tauri/src/summary_stage.rs) | Auto title, model summary и explicit subscription-agent summary; existing agent origin влияет на auto regeneration |
| [stt_stage.rs](../../../src-tauri/src/stt_stage.rs) | Private credential, selected-provider provenance, confirmed live captions и terminal STT state |
| [asr_pick.rs](../../../src-tauri/src/asr_pick.rs) | Persist measured model speed; это тоже writer, хотя относится к выбору модели |
| [video_diagnostic.rs](../../../src-tauri/src/video_diagnostic.rs) | Atomic diagnostic sidecar + `meetings.error`; startup `restore_saved` проецирует saved diagnostic в DB |
| [smoke.rs](../../../src-tauri/src/smoke.rs) | Реальный capture через recorder; optional speaker rename; не isolated service test и не безопасный fixture generator для рабочей библиотеки |

Ключи settings, требующие отдельного переноса: `settings`, `ui_locale` (точные
константы — в модулях), measured-speed keys, `private:soniox_api_key`, `stt:<id>`,
`stt_previous:<id>`, `highlights:<id>`, `speaker_embeddings:<id>`. Разрешение external
principal на библиотеку не открывает private settings, credentials или voiceprints.

## Managed filesystem и внешние outputs

| Владелец сейчас | Файлы и операции | Будущий boundary |
| --- | --- | --- |
| [capture/recorder.rs](../../../crates/bazza-capture/src/recorder.rs), [capture/wav.rs](../../../crates/bazza-capture/src/wav.rs) | mkdir capture directory; `mic.wav`, optional `sys.wav`, `mix.wav`; PCM/header finalization; atomic `mix.clock.json` | GUI raw writer с service lease + OS artifact lock; callbacks не вызывают RPC |
| [capture/video.rs](../../../crates/bazza-capture/src/video.rs), [pipeline.rs](../../../src-tauri/src/pipeline.rs) | FFmpeg пишет `screen.raw.mp4`, clock/log; pause timeline через temp+rename; filter/temp mux и atomic publish `screen.mp4`; после успешного mux удаляются raw/timeline | Fenced worker staging/publication; recovery ждёт освобождения artifact lock, сохраняет raw при ошибке |
| [import.rs](../../../src-tauri/src/import.rs), [share/staging.rs](../../../crates/bazza-share/src/staging.rs), [store/import.rs](../../../crates/bazza-store/src/import.rs), [share/bundle.rs](../../../crates/bazza-share/src/bundle.rs) | FFmpeg conversion либо ZIP extraction в `.bazza-import-*`; OS-locked lease marker, reclaim abandoned stages; `.bazza-import-pending`, rename media, rollback cleanup | Service-owned durable staging/commit; сохранить media allowlist/path/size validation и duplicate-ID skip |
| [commands/meetings.rs](../../../src-tauri/src/commands/meetings.rs) | `remove_dir_all(recordings/<id>)` после DB delete | Tombstone/generation, drain workers/readers, retryable purge; active capture нельзя скрыто останавливать |
| [commands/speakers.rs](../../../src-tauri/src/commands/speakers.rs) | FFmpeg пишет `voice-samples/<id>.pending.wav`, затем rename в WAV; deletion staging/rollback, recovery pending/deleting/orphan files | Service-owned sample lifecycle; GUI inference worker получает ограниченный input/output lease |
| [models/download.rs](../../../crates/bazza-models/src/download.rs), [commands/models.rs](../../../src-tauri/src/commands/models.rs) | `models/*.part`, resume/truncate, checksum, atomic rename либо remove; model removal | Service либо registered worker; loaded models имеют read lease; download только явно |
| [commands/organize.rs](../../../src-tauri/src/commands/organize.rs) | FFmpeg clip output и companion `.txt`; managed `clips/<token>.*`, delete clips; share activation | Durable prepare job и service publication; revoke блокирует activation/new reads |
| [video_diagnostic.rs](../../../src-tauri/src/video_diagnostic.rs) | `recordings/<id>/processing-diagnostics.json`, temp+sync+persist, cleanup после удаления встречи | Одна согласованная operation с DB error projection и generation check |
| [db.rs](../../../crates/bazza-store/src/db.rs), [commands/organize.rs](../../../src-tauri/src/commands/organize.rs) | `bazza.db`/WAL/SHM, directory helpers, library snapshot/recursive copy, `data-dir.txt` | Relocation protocol; наличие snapshot не доказывает согласованность всех media/settings writers |
| [share/bundle.rs](../../../crates/bazza-share/src/bundle.rs), [commands/organize.rs](../../../src-tauri/src/commands/organize.rs) | Явно выбранный export ZIP, clip, text/subtitles, HTML и скопированные media вне library | Explicit export destination; не произвольный filesystem API агента |
| [summary-runner/lib.rs](../../../crates/bazza-summary-runner/src/lib.rs) | Temporary `bazza-summary-*`, CLI-owned `summary.md`, bounded stdout/stderr pipes | Сохранить isolation: empty MCP config, отключённые tools/user config, temporary cwd; интеграция Bazza MCP не передаётся дочернему summary agent |

FFmpeg/ZIP/hound outputs являются filesystem writes, даже если вызывающий Rust-код
не содержит `fs::write`. `bazza-diarize-cli` возвращает JSON в stdout и diagnostics
в stderr; publication результатов в DB выполняет GUI pipeline. Calendar cache сейчас
только в памяти AppState; calendar refresh сам по себе DB не пишет.

## Lifecycle baseline из исходников

1. Startup разрешает data-dir pointer, открывает/мигрирует Store, восстанавливает
   diagnostic projection, читает settings/locale и seed templates. Уже затем Tauri
   подключает single-instance, окна, tray, hotkey; выполняет orphan recovery,
   detector/calendar/engine-idle loops. Enabled shares восстанавливают LAN listener.
2. Close popup/recordbar всегда hide; main hide при `close_to_tray=true` (default).
   Это не Exit и не завершение фоновых потоков/записи. Main close при false передаёт
   стандартное close Tauri; отдельного `RunEvent::ExitRequested` handler в lib.rs нет.
3. Tray Quit при активной записи вызывает `recorder::stop`, затем `app.exit(0)`.
   Ошибка stop здесь игнорируется; не заявляется durable post-processing ACK.
   Share listener/внутрипроцессные queues не переживают смерть GUI.
4. Start сериализован RECORDING_LIFECYCLE, проверяет active recording и data-copy
   guard, сохраняет selected STT provenance до открытия devices/network, пишет raw
   локально независимо от выбранного STT provider. Detection/hotkey/tray предлагают
   picker; не начинают capture молча.
5. Pause/resume согласует audio transition и persisted video timeline; при final
   metadata-write error UI всё равно получает фактическое состояние. Stop закрывает
   и joins audio/video/forwarder/STT/captions, сохраняет meeting/highlights, завершает
   live feed и enqueue post-processing. Ошибка audio сохраняет partial media и failed
   state; late старый forwarder не забирает новую recording session.
6. Recovery рассматривает recording/processing rows: missing/unusable audio даёт
   failed, usable recording получает duration и processing, затем recovery pipeline.
   Performance queue дедуплицирует meeting IDs и сериализует heavy jobs в памяти;
   durable MCP jobs/leases/fencing из неё автоматически не следуют.
7. Engine idle unloader пропускает активную recording и `busy>0`; cached engines
   выгружаются после 10 минут бездействия. Сохранить busy_guard при worker cutover.
8. Soniox revoke/provider change/key clear держат lock order settings → cancel →
   persistence и отменяют registered cloud worker; local writer остаётся независим.
   Собственный service control-channel outage protocol ещё предстоит реализовать.

Текущие defaults, которые нельзя менять под видом MCP: local STT, Soniox consent
false, system capture true, app audio scope, video enabled 15 FPS/1080p,
close-to-tray true, explicit user recording/share/name actions. Это source baseline;
реальный start/pause/stop и process-tree kill требуют отдельной runtime проверки.

## Протокол service startup: точка переноса

До GUI cutover новый service следует запускать только с disposable synthetic
library. Будущий production порядок из плана: canonicalize existing library →
per-user/library singleton owner → проверить schema/build/API identity → открыть
Store и migrate → атомарно опубликовать private endpoint → authenticated handshake.
Второй launcher подключается к owner; stale discovery не разрешает direct SQLite
fallback. Public connection ID не является credential. Точная реализация bootstrap
и detached process flags принадлежит service-client, а не данному аудиту.

Обычный `spawn`, скрытое окно или успешный EOF adapter не доказывают независимость
от job/process group реального host. Windows/macOS host kill-tree acceptance остаётся
отдельным gate. Legacy GUI не знает нового ownership lock: production installation
interlock и остановленный старый GUI обязательны для первого cutover.

## Повторение аудита и критерий zero-write

Из корня repository (PowerShell, rg):

~~~powershell
rg -n 'Store::|Arc<Store>|Connection::|rusqlite' src-tauri crates --glob '*.rs'
rg -n '\bstore\s*\.|\.store\s*\.' src-tauri/src --glob '*.rs'
rg -n 'set_setting|\.save\(|seed_templates|snapshot_to|write_data_dir_pointer' src-tauri/src crates/bazza-store/src --glob '*.rs'
rg -n 'fs::|File::|OpenOptions|WavWriter|NamedTempFile|tempdir_in|persist\(' src-tauri/src crates --glob '*.rs'
rg -n 'Command::|run_ffmpeg|\.arg\(|\.args\(' src-tauri/src crates/bazza-capture/src crates/bazza-share/src --glob '*.rs'
rg -n '\.store\(' src-tauri/src crates --glob '*.rs'
rg -n 'CloseRequested|ExitRequested|\.exit\(|recover_orphans|ShareServer::start|fn start|fn stop|fn pause|fn resume' src-tauri/src --glob '*.rs'
~~~

Результаты включают tests: классифицировать production часть до соответствующего
`#[cfg(test)]`, а tests/examples отдельно. `.store(value, Ordering::...)` у atomic
flags/counters/clock — запись в память, не persistence. `state.store.method(...)`,
`Settings::save` и filesystem operations — persistence независимо от имени caller.
Одного поиска `.store()` недостаточно; chained/multiline calls и subprocess outputs
проверяются по исходникам и таблицам выше.

Для этапа 3 gate: production src-tauri не содержит Store constructors/connections/
`Arc<Store>`, все перечисленные managed writes выполняет service либо fenced worker,
recovery не действует при живом artifact owner, GUI не имеет DB fallback, а GUI +
два adapters проходят concurrent commit/restart/revoke tests. Этот аудит фиксирует
исходную поверхность переноса и не объявляет gate выполненным.

## Изменения этапа 1

[Отчёт](../test-reports/2026-11-02-mcp-phase-1.md) уточняет исходные TODO выше:

- `Store::open` остаётся legacy GUI entry point, но теперь проверяет future schema
  до WAL/migrations. `schema_version` сначала читает bounded SQLite header; старый
  header проверяется обычным SQLite read с учётом committed WAL. `migrations::run`
  дополнительно отклоняет future schema. Миграции 1–9 не переписаны.
- [owner.rs](../../../crates/bazza-store/src/owner.rs) и owned create/open APIs
  удерживают canonical guard до закрытия Connection. Их новый caller —
  [Repository::open_fixture](../../../crates/bazza-core/src/lib.rs); production GUI
  ещё не переведён на него. `create_owned` вызывается явно и проверяется unit/
  integration tests, а service start использует только RequireCurrent.
- Новые managed metadata writers: core создаёт `.bazza-library-identity.json`
  после schema validation под Store owner; host атомарно публикует/удаляет свой
  `.bazza-service-endpoint.json` под тем же lifetime guard. Metadata read не вызывает
  `recordings_dir`/`models_dir`, не создаёт media directories и не запускает recovery.
- [Broker](../../../crates/bazza-service-client/src/broker.rs) пишет ограниченный
  nonsecret `.bazza-service-broker.json`, удерживает отдельный setup lock и меняет
  только verified current-user demand task при явном local control. Это development
  fixture path, не внешний MCP tool и не production library grant.
- Transport создаёт local endpoint (на macOS также private socket directory/lock)
  и проверяет OS peer. Activity counters/atomics host — только память; они не
  заменяют durable jobs, change log, idempotency или worker fencing.
- Согласованный Nemotron hunk `BAZZA_DATA_DIR` выбирает explicit absolute directory
  в окружении тестового GUI, до normal pointer lookup; normal unset behavior
  сохранено. MCP host/client используют explicit root, не этот selector.

Новые host/core открывают только помеченные synthetic fixtures. Эта отсечка
сохраняется до stage-3 stopped-owner/version interlock; нельзя считать legacy
GUI исключённым одним advisory lock. После параллельных pipeline/sidecar/model
изменений Nemotron перед следующим cutover повторить production writers inventory.

## Повторный source audit перед этапом 2

Повторён 2 ноября 2026 года по текущему общему дереву после handoff Nemotron,
capture/video и performance-сессий. Это read-only проверка исходников, без запуска
GUI, записи, inference или открытия пользовательской библиотеки. Исходные таблицы
выше остаются полезной картой; следующие уточнения относятся к текущему snapshot.

| Текущий участок | Уточнённый writer / lifecycle boundary | Следствие для этапов 2–3 |
| --- | --- | --- |
| [lib.rs](../../../src-tauri/src/lib.rs), [state.rs](../../../src-tauri/src/state.rs) | GUI по-прежнему вызывает `Store::open`, затем `restore_saved`, создаёт `Arc<Store>` и seed templates; recovery и enabled integrations запускаются в прежнем GUI lifecycle. | Новая service schema сама по себе не переводит GUI под одного owner. Production MCP остаётся закрыт до полного cutover. |
| [pipeline.rs](../../../src-tauri/src/pipeline.rs), [state.rs](../../../src-tauri/src/state.rs), [diarize-cli](../../../crates/bazza-diarize-cli/src/main.rs) | Nemotron выбирает отдельный native runtime и один непрерывный sidecar stream; legacy backend сохраняет shards. Sidecar возвращает turns/voiceprints через stdout, а GUI сохраняет segments, meeting speakers и `speaker_embeddings:<id>`. Выбор Nemotron не создаёт второго DB writer внутри CLI. | Будущий worker commit должен одним fenced действием опубликовать transcript/speaker provenance; не менять backend, TitaNet identity или пользовательское подтверждение имени ради MCP. |
| [pipeline.rs](../../../src-tauri/src/pipeline.rs), [meetings.rs](../../../crates/bazza-store/src/meetings.rs) | Pipeline публикует terminal state через targeted `update_processing_state`; full-row `update_meeting` остаётся у других callers, например rename. Замена segments, `stt_previous`, STT provenance, meeting speakers и embeddings выполняются отдельными Store calls. | Targeted state update сохраняет параллельные metadata edits, но не даёт общей transaction, revisions, idempotency или immutable source history. Эти гарантии нельзя приписывать legacy pipeline. |
| [processing_jobs.rs](../../../src-tauri/src/processing_jobs.rs), [commands/meetings.rs](../../../src-tauri/src/commands/meetings.rs), [commands/organize.rs](../../../src-tauri/src/commands/organize.rs) | Очередь сериализует heavy jobs в памяти; `contains` и RAII `reserve` защищают queued/running meeting от destructive GUI operation. Library copy проверяет queue/busy и использует `data_copy`; startup recovery восстанавливает работу по meeting status. | Сохранить admission/reservation при этапе 3. Это не durable registry, worker lease или fencing token; stage-2 jobs нельзя выдавать за уже перенесённую очередь. |
| [video_diagnostic.rs](../../../src-tauri/src/video_diagnostic.rs), [pipeline.rs](../../../src-tauri/src/pipeline.rs) | Диагностика хранит video warning отдельно от optional AI errors, атомарно публикует bounded `processing-diagnostics.json`, затем проецирует `meetings.error`. Есть повторная проверка существования meeting и cleanup собственного позднего файла. Startup `restore_saved` также пишет DB; успешная проверка видео отдельно снимает warning. | Sidecar + DB остаются двумя публикациями с process-local lock. Не стирать новую диагностику при общих metadata/processing writes; future tombstone/generation gate должен охватывать обе публикации и deferred AI retries. |
| [segments.rs](../../../crates/bazza-store/src/segments.rs), [speakers.rs](../../../crates/bazza-store/src/speakers.rs), [ai.rs](../../../crates/bazza-store/src/ai.rs), [vectors.rs](../../../crates/bazza-store/src/vectors.rs) | Manual correction и automatic cleanup сохраняют raw, manual empty и protected manual overlays; chunks перестраиваются внутри их transaction. Legacy `replace_segments` и `set_segment_speakers` не выполняют общий revision/source protocol; pipeline отдельно rebuilds chunks. Conditional vector write сверяет current chunk text. | Stage-2 batch corrections должны валидировать весь batch до write, перестраивать chunks один раз и обновлять новую search projection в той же transaction. Legacy raw+clean FTS query не заменяет effective-only поиск новой границы. |
| [summary_stage.rs](../../../src-tauri/src/summary_stage.rs), [ai.rs](../../../crates/bazza-store/src/ai.rs) | Summary generation по-прежнему читает transcript/highlights и пишет `set_summary`; специальная защита существующего agent summary проверяет legacy structured marker. Automatic title тоже пишет Store. Persistent deleted/absent generation и typed `auto_replace` policy legacy caller не применяет. | Новые summary states/history/suppression должны быть проверены domain API; stage 3 переносит каждый automatic/explicit generation commit на expected source/target revisions. |
| [commands/meetings.rs](../../../src-tauri/src/commands/meetings.rs), [lib.rs](../../../src-tauri/src/lib.rs) | Media read теперь отдельно разрешает Tauri asset scope каждому существующему published leaf; audio всё ещё обязательная строка пути. `get_meeting` вызывает legacy `recordings_dir`, который создаёт directory. | Service text-only read не должен вызывать этот helper. Optional media и UI routing остаются этапом 8; stage-2 text meetings пока не предназначены для legacy GUI. |

Проверены также model download/checksum helpers: managed model writes остаются
явным download/remove workflow, новые runtime/model probes читают локальные файлы.
Stage-2 fixture tests не должны вызывать model lookup, download, ffmpeg, sidecar,
recovery или global data-dir selection. `BAZZA_DATA_DIR` остаётся opt-in GUI selector;
service использует явный root.

Frozen v8 fixture по-прежнему создаётся из отдельного `schema_v8.sql`, публикует
marker последним и отказывается от любого существующего root. Migration test
сравнивает прежние столбцы 16 legacy tables и media bytes, поэтому новые tables /
columns не должны менять его baseline. Добавить отдельные assertions для новых
baseline revisions/source states/search readiness, а не переписывать frozen v8 SQL.

Граница реализации этапа 2: новые migrations и transactional domain operations
проверяются на disposable libraries; существующие GUI/capture writers сохраняются.
Пока они не переведены на этот protocol, domain revisions/search projection не
объявляются согласованными со всеми production writers. Перед включением
production повторить zero-write gate и native recording/capture regression на
итоговом дереве; результаты параллельных сессий не являются MCP cutover acceptance.

## Первый increment этапа 3

[Проверка](../test-reports/2026-11-02-mcp-phase-3.md) добавляет новую, пока
fixture-only поверхность writers; существующий GUI не подключён к ней:

- `service_data/workers.rs` регистрирует PID/epoch/token-hash worker, создаёт
  recording row/lease/selected-provider setting и атомарно публикует confirmed raw,
  provenance, lifecycle revision, post-process job и finalize receipt. Минимальная
  capture identity остаётся после meeting purge, исключая late-prepare resurrection.
- `bazza-capture-handoff` пишет persistent artifact lock/owner identity,
  framed confirmed-STT journal и versioned terminal manifest. Guard охватывает
  close/join/fsync caller; capture callbacks не вызывают эти операции. Caller
  передаёт только существующий UUID directory; default library lookup отсутствует.
- `bazza-core::worker` проверяет authenticated peer/session/fence до artifact I/O,
  создаёт только managed fixture recording directory, требует released artifact
  guard и sealed terminal перед finalize. Readonly committed receipt replay не
  читает удалённые файлы и не разрешает old-epoch mutation.
- Service startup/periodic monitor читает pending capture state и проверяет OS
  ownership. Busy остаётся active; Free без terminal — pending. Recovery не делает
  inference и не основан на TTL. Idle boundary использует свежий owner probe,
  monitor drain завершается до release Store owner.

Эти функции не заменяют recorder/STT/cloud cancellation/pipeline/import/voice/model
writers из исходной таблицы. Current `src-tauri/src` совпал по SHA с baseline этого
increment. Nemotron-сессии вновь согласован quality hunk в diarize/CLI/protocol и
pipeline voice matching; перед GUI cutover обновить inventory по её итоговому коду.
Нельзя объявлять zero-write gate по наличию нового private worker API.

## Второй increment этапа 3

Новые `service_data/desktop.rs`/`bazza-core::desktop` публикуют только title metadata
и три preference fields через existing mutation transaction: expected revision,
change event и minimal replay receipt. Stable trusted principal `desktop` не
зависит от PID окна; caller не выбирает role/actor. Title rename во время capture
не обновляет lifecycle/status/participants/raw/processing fields. Новая v12 table
хранит только preference revision; legacy settings сохраняют чужие ключи.

`RepositoryClient` не зависит от Store/SQLite и возвращает identity для всех
snapshots/pages. Read-only projections не создают media/model directories; media
flags проверяются после DB lock. Это новая fixture-only поверхность, существующие
GUI writers ещё обходят её и не попадают автоматически в revision/change feed.
Нужны адаптеры, invalidation/refetch и stale-response ordering перед cutover.

Optional-media изменения относятся к `app/src`: nullable audio, player lifecycle,
отключённые media-only actions и отдельный text-only mock. Production native
`get_meeting` пока возвращает прежнюю audio string и использует legacy directory
helper. Полный native text-only route ещё не объявлен готовым. MCP не менял
`src-tauri/src` в этом increment. Nemotron параллельно изменяет согласованные
pipeline voice matching и `commands/speakers.rs::rename_speaker` query-cache seams;
её улучшения нужно сохранить при переносе writers. SHA baseline первого increment
не является доказательством неизменности всего общего дерева после этих работ.

После финального MCP прогона voice reliability hardening добавил централизованный
`pipeline::speaker_embeddings_key`: `speaker_embeddings:<POLICY_ID>:<meeting_id>`.
Согласованный rename hunk и один read-only key hunk в smoke используют тот же helper;
unversioned queries не должны обучать Voices под новой policy. Это работа
Nemotron-сессии, её native acceptance не заменяет MCP cutover regression.

Проверка integration cleanup выявила оставшийся gap: legacy `delete_meeting`,
service meeting delete и transcript replace/delete удаляют stt/stt_previous, но
не scoped embedding caches. При переносе writers очистить legacy и versioned
meeting caches с точным сопоставлением ID; не использовать неэкранированный LIKE
и не удалять global voice profiles/samples. Meeting deletion также должна очищать
highlights. Во время native build freeze Store/production code не менялись;
это зафиксированная работа перед cutover, а не выполненная гарантия cleanup.
Обычные post_process/rediarize предварительно очищают current-policy cache с
проверкой результата; риск stale vectors здесь ограничен прямыми Store/service
replace/delete paths, обходящими эту оркестрацию. Legacy replace_segments также
не очищает settings. Общий transaction-local helper нужен в обоих слоях;
cleaned-text-only edits должны сохранять voice cache.

## Закрытие cache gap и подготовка fixture GUI

В следующем increment общий `settings::clear_meeting_voice_queries_on` подключён
в существующие transactions legacy delete_meeting/replace_segments и service
meeting delete/transcript replace/delete. Сопоставление literal namespace и полного
ID не использует LIKE/GLOB или suffix match; незнакомые настройки, другие встречи,
profiles и samples сохраняются. Meeting delete очищает также точный highlights key.
Legacy STT semantics и clean-only overlays не менялись. Это закрывает gap выше;
не является переносом всех legacy writers под service revisions.

Новый native get_meeting читает media paths без mkdir и возвращает optional audio;
существующие published media получают прежние leaf asset grants. Реальная запись,
recorder/STT/pipeline/speakers/smoke этим MCP increment не менялись.

Повторный inventory: 74 различных прямых Store methods в 18 production-файлах
src-tauri, smoke — 19-й файл без новых методов. Transitive share/import/model file
writers считаются дополнительно. Двадцать serde value types теперь извлечены в
bazza-domain; Store re-exports сохраняют callers, SQL mappings и unknown stored
status fallback остались в persistence layer. Private NativeSettings сохраняет
38 native fields/defaults без capture/model engine dependency. Это не public DTO:
ключи, URLs и voice vectors нельзя экспортировать через public MCP projections.

Private configuration read/update согласован с narrow preferences общей durable
revision и hash точного legacy settings blob. Unknown JSON values сохраняются,
receipts/events не содержат settings. Provider/consent mutation запрещена до
distributed cancellation protocol. Private bounded hotkey guard сохраняет доступ
к bootstrap; прежняя native validation не менялась. Settings::save,
dismiss_detection("never") и cloud registration ещё используют legacy Store.

Append v14 покрывает folder/tag CRUD, atomic folder update/template assignment,
NOCASE named tags и meeting associations; revisions независимы от retention.
Append v13 покрывает unused prepare abort/startup compensation и обязательный
begin под живым kernel-authenticated guard. Terminal seal запрещает новый begin,
но разрешает control reattach/finalize. Реальный recorder ещё не использует seam.

Windows video теперь использует atomic private Job Object assignment при
CreateProcess; synthetic owner-crash/descendant/stderr/Drop checks и FFmpeg helper
прошли. macOS ещё не связывает child lifetime. Free handoff owner может опередить
завершение Windows job children и не доказывает writer-drained состояние.
Automatic unsealed recovery не включать до отдельного drain barrier и macOS
эквивалента. Fixture failed-finalize теперь сохраняет inputs и статус Failed
без постановки job, как и native writer failure. Старые receipts остаются
читаемыми.

Fixture GUI должен входить до data_dir/Store::open/AppState::new и обходить весь
normal setup: recovery, seed templates, detector/calendar/share/hotkey/smoke.
Отдельные identifier, state и invoke whitelist необходимы; одна BAZZA_DATA_DIR или
новая route недостаточны. Проверка synthetic marker обязательна также при уже
работающем service. Frontend entry не должен вызывать legacy initializeLocale.
Packaged v12 fixture acceptance этой ветки пройдена отдельно; production gate
открыт. Последующие v13/v14 source changes имеют отдельные process/native checks.

Schema15 добавляет bounded private templates со staged body writes и retained
upload identity, а также registered post-process list/claim/renew/fail. Эти вызовы
не исполняют pipeline и не публикуют его результат. Ordinary GUI Store всё ещё
сохраняется. Новые startup interlock и open_legacy gate защищают текущий build:
обычная библиотека не получает service DDL выше v9 неявно, service identity
блокирует legacy reopen. Исторические executable не знают этого interlock.

`bazza-share` теперь разделяет portable domain/html/subtitles и default backend
со Store/server/bundle/staging; native ещё включает backend. Это подготовка
dependency boundary, а не принятый zero-SQLite gate. Следующий участок —
lease/fence-bound transcript input; затем нужны media leases, output publication
и перенос остальных enumerated writers с UI revisions/invalidation.
