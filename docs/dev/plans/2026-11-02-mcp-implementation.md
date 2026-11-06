# План реализации MCP для Bazza

Статус: active. Этапы 0–2 проверены в доступном Windows scope. Этап 3 включает
private GUI/worker RPC, append schema23, isolated capture handoff и запуск обычных
окон на тестовом владельце библиотеки. Параллельный
срез adapter даёт девять read-only MCP tools на synthetic library через RMCP 3.8.0.
Windows Inspector прошёл обе версии протокола. Production GUI/worker cutover,
grants, public mutations и приёмка реальных hosts остаются открыты.
Владелец: текущая сессия MCP — API/core/service/client/transport, capture-handoff, Store ownership/schema/data core, optional-media UI, изолированный fixture GUI и план. Работа в существующем dirty tree без переключения ветки. Nemotron завершила quality task и освободила seams; её pipeline/rename/smoke voice hardening сохраняется. Frozen runtime/evidence остаются историческим baseline. Её disposable native target согласован для test-only MCP проверок; обычный target/runtime не меняется. Performance-test чат archived. Lean target изолирован. Ветка codex/mcp-local-service не создавалась.
Дата: 2 ноября 2026 года.

[Исследование и уточнённые требования](../research/2026-11-02-mcp-integration.md) · [Архитектура сейчас](../architecture.md) · [Правила работы](../agent-workflow.md) · [Проверки](../testing.md)

## 1. Результат и границы

Внешний AI-клиент получает через MCP доступ ко всем продуктовым данным выбранной библиотеки Bazza: текущим и будущим встречам, транскриптам, саммари, собственным заметкам, участникам, организации библиотеки и sharing. Разрешение выдаётся один раз на библиотеку и выбранный набор операций. Дополнительного разрешения на каждую встречу или каждую уже разрешённую mutation сервер не требует.

GUI может полностью завершиться. AI-клиент запускает лёгкий stdio adapter; тот подключается к самостоятельному пользовательскому процессу bazza-service или запускает его без окна. Сервис владеет хранилищем и выполняет CRUD/search/sharing без Tauri.

### Обязательные требования

| ID | Наблюдаемое поведение |
| --- | --- |
| M1 | Один library grant разрешает чтение/создание/изменение/удаление/sharing всей текущей и будущей библиотеки; нет per-meeting consent |
| M2 | При отсутствии процесса GUI работают чтение, CRUD, поиск, календарный cache и управление shares |
| M3 | Создаются текстовые карточки встреч; изменяются metadata и cleaned transcript; возможны явный import/replace/delete транскрипта и удаление встречи со связанными данными |
| M4 | Собственные заметки имеют persistent CRUD и отделены от AI live notes |
| M5 | Саммари можно прочитать, явно создать/заменить и удалить с контролем версии и provenance |
| M6 | Один поиск охватывает title, effective transcript, current summary и user notes; результат указывает источник |
| M7 | Share можно создать, изменить, отключить и удалить; ссылки обслуживаются при закрытом GUI |
| M8 | GUI, два агента и фоновые workers работают через согласованные revisions, jobs и invalidation |
| M9 | Raw transcript сохраняется при редактировании; существующие библиотеки и recorded meetings мигрируют без потери исходных данных |
| M10 | Ошибки, повтор запроса и restart не дают двойных mutations, потерянных обещанных jobs или позднего восстановления удалённого объекта |
| M11 | Локальная установка и native acceptance проверены отдельно на Windows и macOS, в конкретных версиях Codex и Claude Code |

Календарь в базовом объёме — чтение persistent cache и refresh уже настроенных ICS-источников. CRUD событий у Google/Outlook требует отдельного provider API и не выводится из разрешения на локальную библиотеку.

Доступные общие встречи: собственные исходящие shares и импортированные .bazza доступны в базовом объёме. Для встреч, которыми поделились другие без импорта, предусмотрен отдельный этап совместимого read-only shared-source catalog. Он учитывает права источника; library grant не создаёт прав у чужого владельца.

Секреты, private settings, voiceprints и произвольные filesystem/SQL/shell операции не входят в публичные MCP DTO. Создание карточки встречи не начинает capture. Существующие правила записи, подтверждения voice identity и изоляции summary-runner сохраняются.

Работа локальная: план не предусматривает remote repository, push, PR, публичную публикацию сервера или распространение fork.

## 2. Архитектура и владение

~~~mermaid
flowchart LR
    A["Codex / Claude Code"] -->|MCP stdio| M["bazza-mcp"]
    M -->|private RPC + library grant| S["bazza-service"]
    G["Bazza GUI"] -->|trusted application RPC| S
    W["GUI capture / inference workers"] -->|lease + fenced commits| S
    S --> C["Tauri-free application services"]
    C --> D["Store: bazza.db"]
    C --> F["Managed files / index / job state"]
    S --> H["LAN share server"]
    S --> K["Calendar cache / refresh"]
~~~

### Решения для реализации

1. Один bazza-service на OS-пользователя и каноническую библиотеку. Только он открывает production Store, запускает migrations и изменяет БД.
2. GUI и MCP используют общее Tauri-free API. Production GUI не имеет fallback к прямому Store при недоступности сервиса.
3. Native capture, Tauri windows, detector, hotkeys и загруженные AI engines остаются в GUI. Worker получает ограниченную lease на raw/staged artifacts и публикует результат через сервис.
4. Фоновый сервис не включает запись, discovery окон, cloud STT или model download при простом открытии библиотеки.
5. Отсутствие GUI — штатный режим. GUI-зависимые функции могут сообщать GUI_UNAVAILABLE; data CRUD не возвращает прежний APP_NOT_RUNNING.
6. Сервис живёт независимо от process tree adapter/GUI. EOF stdio завершает adapter; исполняемые service jobs и активный sharing продолжаются. GUI jobs без исполнителя честно ожидают worker.
7. Сервис запускается по требованию. Он остаётся жив, пока есть clients, executable jobs, активные shares, worker leases или явно включённая фоновая календарная синхронизация. Waiting-for-GUI jobs сами по себе не запрещают idle shutdown.
8. Публичный вход первого выпуска — stdio. Private IPC — Windows named pipe / macOS Unix domain socket. Public Streamable HTTP и remote tunnel не требуются для этого результата.

Полный перенос inference/capture в daemon — отдельная возможная работа. Для CRUD пользователь может передать готовый текст саммари от своего агента. Headless-генерация саммари локальной моделью или подписочным CLI не является зависимостью базового выпуска.

### Предлагаемая структура

Названия новых пакетов — проектные.

| Место | Ответственность |
| --- | --- |
| crates/bazza-service-api | IDs, revisions, DTO, errors, RPC envelopes/versions, worker/admin/external contracts |
| crates/bazza-core | Tauri-free application operations, authorization policy, coordinated mutations, jobs/file lifecycle |
| crates/bazza-service | Singleton host, local IPC, Store owner, calendar/sharing/job executors |
| crates/bazza-service-client | Authenticated IPC client, bounded calls, bootstrap/discovery, change subscription |
| crates/bazza-mcp | RMCP handlers, tools/resources, stdio lifecycle; использует service-client |
| crates/bazza-store | Append migrations, transactional repositories, FTS, revision/idempotency/change helpers |
| crates/bazza-share | Read-only LAN publication, bundles и safe staging; вызывается сервисом |
| src-tauri/src/state.rs и commands | RepositoryClient вместо Arc<Store>, thin application wrappers |
| app/src/lib/ipc.ts и UI | Настройки интеграции, notes, optional media, invalidation, typed errors |
| scripts/ и Tauri bundle config | Локальная сборка и согласованная упаковка service/adapter/GUI |

Внутренние роли RPC разделяются: application client, registered worker, administration и external MCP principal. External credential не вызывает private settings, worker publish, data-dir migration или administration. Один generic invoke(method,args), доступный агенту, не создаётся.

## 3. Требования к данным и контрактам

### Whole-library grant

- При включении интеграции человек разрешает чтение, создание, изменение, удаление и управление sharing выбранной библиотеки. Область включает будущие встречи.
- Grant хранит library_id, principal/client identity, operation permissions, created/revoked state и credential reference.
- Полномочия проверяет service на каждом запросе и перед отложенным side effect; adapter не является единственной точкой проверки.
- Нет allowlist отдельных встреч, UI approval queue для разрешённых CRUD и параметра confirmed=true.
- Credential создаётся pairing flow и хранится в Windows Credential Manager/macOS Keychain; config содержит public connection ID. Окружение — поддерживаемый advanced вариант.
- Локальные admin CLI-команды позволяют посмотреть подключения и отозвать grant без GUI. Эти команды не публикуются как MCP tools.
- Host может иметь собственные approval policies; сервер Bazza не обещает отменить их.
- UI объясняет, что результаты MCP получает выбранный agent host, который может обрабатывать их внешним провайдером.
- Защита не является sandbox от полностью скомпрометированного процесса того же OS-пользователя.

При revoke ещё не committed queued external operations отменяются, running получают cancellation intent. Проверка grant/permission epoch и mutation commit либо share activation выполняются в одной сериализованной транзакции/критической секции с revoke. Для внешнего эффекта фиксируется точка начала: после committed revoke новые эффекты не начинаются; уже переданные данные отозвать невозможно. Уже committed изменения сохраняются, а обязательный cleanup/purge продолжается от имени сервиса. Отзыв подключения по умолчанию также отключает созданные им активные/preparing shares; локальный admin может явно сохранить конкретные shares.

### Revisions и idempotency

Использовать отдельные opaque revisions: metadata, transcript, summary, каждая note, share и lifecycle state. Semantic change, revision increment, change event и idempotency outcome фиксируются одной транзакцией.

Все write/delete/start-job tools принимают idempotency_key. Key связан с principal, library, tool и hash нормализованных аргументов. Одинаковый key+payload возвращает прежний outcome; другой payload — IDEMPOTENCY_CONFLICT. Хранить минимальный outcome с IDs/revisions, а не копии удаляемых summary/note bodies. Срок deduplication документировать и возвращать в результате; после его истечения не обещать прежнюю гарантию повтора.

Update/delete требуют expected_revision. Summary и transcript имеют persistent state revision даже в состоянии absent/deleted: чтение возвращает эту revision, а create/replace/delete проверяют её. Для write_summary mode=create требует отсутствующего current, mode=replace — существующего; оба требуют expected_revision текущего summary state. Это предотвращает ABA после create→delete. JSON-RPC id не является idempotency key.

Для связанных полей использовать typed patch и конкретные add/remove tag operations. Full-row read-modify-write Meeting из нынешнего rename_meeting заменить targeted mutation.

### Jobs

Состояния: queued, waiting_for_worker, running, succeeded, failed, cancelled, interrupted; cancel_requested хранится отдельным признаком до фактического terminal outcome. Service executors исполняют cache/share/file operations; GUI workers — требующие native engines jobs. Durable handle создаётся до ответа. Checkpoint/claim lease и fencing позволяют безопасный reclaim после restart. Waiting-for-worker не выдаётся за running и само по себе не удерживает idle service.

### Append migrations

Baseline исходного плана — v8; performance-сессия добавила v9. После проверки дерева этап 2 добавил v10, сохранив v1–v9. Последующие schema changes также append-only. Identity sidecar остаётся из этапа 1; grants, registered worker sessions, managed share/received-source schemas добавляются в своих последующих этапах.

| Логический пакет | Схема / назначение |
| --- | --- |
| Identity и control | library identity/epoch, schema compatibility range; grants без открытых credentials |
| Consistency | entity revisions; monotonic library_changes; idempotency_records |
| Durable execution | jobs, worker sessions/leases, deletion_operations, operation staging/outbox |
| Transcript sources | Immutable bounded raw source payloads, current-source pointer, persistent transcript state/revision/deletion epoch; segments остаётся текущей проекцией |
| Text meetings и notes | content_kind=recorded/text, nullable media в DTO; user_notes с UUID, optional meeting_id, title/body/revision/times/provenance |
| Summary | Persistent summary state/revision, включая absent/deleted и suppression; typed origin/auto_replace policy; immutable summary_history для замен |
| Calendar | calendar_sources identity, cached occurrences и sync state; source secret URL остаётся private |
| Search | search_documents + FTS5, source identities/revisions/representation, search generation/readiness |
| Sharing | public share_id отдельно от token; mode, fields/media policy, revisions, creator grant, status, expiry, snapshot reference и publication_dependencies |
| Received shared sources | Source/access/cache/freshness model для отдельного совместимого read-only обмена |

Notes выбранной модели допускают несколько документов встречи и самостоятельные заметки. Привязанная note удаляется вместе со встречей; самостоятельная не затрагивается. Перенос привязанной note в самостоятельную выполняется явной update.

Существующие встречи backfill как recorded. Отсутствие mix.wav не превращает повреждённую запись в text meeting. Исходные raw/clean/manual flags, media hashes, participants, speaker memory, settings и structured summary сохраняются.

Summary history создаётся при замене; current projection остаётся удобной для существующего UI. Origin: legacy/imported/manual/external_agent/configured_generation. Для ручного или внешнего summary auto_replace=false. Delete summary удаляет current и его сохранённые версии, убирает search projection, увеличивает persistent state revision и сохраняет automatic_generation_suppressed=true вне удалённой row. Поздний/будущий automatic pipeline не воссоздаёт документ; восстановление — explicit write/regenerate. Worker фиксирует input и target state generations до inference; устаревший результат не затирает текущий.

Transcript_sources хранит неизменяемый исходный timed payload и безопасную provider provenance, а meeting_transcript_state — current source ID, revision и deletion epoch. Existing raw backfill сохраняется как legacy source. Replace создаёт новый source и атомарно перестраивает текущие segments/chunks/FTS; старый raw остаётся архивным источником и не попадает в default search. Delete transcript удаляет current segments и все transcript source payloads/provider caption copies/overlays/chunks/vectors, оставляя только state generation для fencing. Обычное редактирование меняет только cleaned overlay.

Календарный cache ключуется source_id + UID + occurrence start. Refresh парсит вне DB lock и транзакционно заменяет события одного источника. Ошибка источника сохраняет последний удачный cache со stale flag. Не обещать полной recurrence-поддержки, которой нет у существующего parser.

## 4. Публичный MCP API

Конкретные имена фиксируются на этапе 0. Таблица определяет функциональный объём, а не существующие команды.

| Группа | Tools | Контракт |
| --- | --- | --- |
| Состояние | bazza_get_status | library/API/service versions, index/job/share health и capabilities; без secrets |
| Библиотека | bazza_list_meetings, bazza_get_meeting, bazza_get_library_structure | Metadata, participants, revisions, folders/tags, доступность содержимого; cursor/limit |
| Встречи | bazza_create_meeting, bazza_update_meeting, bazza_delete_meeting | Text meeting, typed metadata patch, coordinated delete |
| Транскрипт | bazza_get_transcript, bazza_import_transcript, bazza_replace_transcript, bazza_set_transcript_corrections, bazza_delete_transcript | Current/raw/effective source, explicit source replacement/deletion, atomic overlays/reset, source-state revision |
| Саммари | bazza_get_summary, bazza_write_summary, bazza_delete_summary | Create/replace/delete с revision и provenance |
| Заметки | bazza_list_notes, bazza_get_note, bazza_create_note, bazza_update_note, bazza_delete_note | Persistent Markdown, optional meeting association, revision |
| Поиск | bazza_search | fields=title/transcript/summary/notes, filters, representation, citations и diagnostics |
| Календарь | bazza_list_calendar_events, bazza_refresh_calendar | Чтение cache и explicit refresh configured sources; источник и freshness |
| Sharing | bazza_list_shares, bazza_create_share, bazza_update_share, bazza_delete_share | Meeting/clip, live/snapshot, публикуемые поля/media, expiry/status/revision |
| Jobs | bazza_get_job, bazza_cancel_job | Public job projection, stage/outcome, cancellable policy и фактическая отмена |
| Полученные встречи | bazza_list_shared_meetings, bazza_get_shared_meeting, bazza_refresh_shared_source | Отдельный этап: configured compatible sources и их cache |

Create meeting принимает metadata и optional готовый timed transcript. Базовый контракт не фабрикует таймкоды из plain text: нетаймированный текст можно сохранить собственной заметкой. Расширение на untimed transcript потребует отдельной модели и не меняет формат существующих timed segments.

Import transcript применим к существующей text/recorded встрече без current transcript, включая импорт готовых субтитров. Replace — явная установка нового source, с архивированием прежнего raw и осознанным сбросом overlays, что отражается в описании tool. Оба используют настоящие timed segments, expected transcript state revision и не исполняются при активном capture/pipeline lease. Delete удаляет все payloads транскрипта, включая provider caption copies и архивы; media, notes и summary остаются отдельными сущностями. Summary, ссылавшееся на удалённый source, получает source_missing; automatic generation подавляется до явного действия. Outcome перечисляет retained entities, старые citations становятся unavailable.

Public job DTO содержит executor_kind, status, stage, cancellable и cancellation_reason. Отмена через MCP разрешена только явно перечисленным видам внешних операций. Capture/recovery и обязательный cleanup committed mutations не отменяются этим tool. Cancel share preparation отзывает preparing publication; cancel purge после logical delete отклоняется. Внутренние worker/control методы не становятся доступными по известному job ID.

Существующие folder/tag CRUD можно позже опубликовать отдельными предметными tools. В базовом выпуске get_library_structure позволяет выбрать существующие папки/теги, update_meeting — назначить/снять их; UI сохраняет прежние операции организации.

Resources добавляются как удобный альтернативный путь к тем же metadata/summary/note/segment DTO и той же policy. Ключевой сценарий работает только через tools. Prompts необязательны. Большие transcript/note/summary читаются bounded pages; URI не становится filesystem path.

### Формат запроса и результата

Object-shaped input/output, JSON Schema 2020-12, bounded strings/arrays, additionalProperties=false на входе. Единый output envelope допускает и success, и domain error, чтобы outputSchema не противоречил ошибкам. Domain error: isError=true; protocol errors оставляет SDK.

Минимальные поля результата: schema_version, library_id, data либо error, warnings. Машинная ошибка содержит code, безопасное message, retryable, optional retry_after_ms/current_revision. Public DTO не включает absolute media paths, credentials, private calendar URLs и bearer tokens без операции, явно возвращающей share URL.

Начальные лимиты для проверки: search limit=20/max=100; transcript page до 100 сегментов или 64 KiB, note/summary page до 64 KiB; bounded mutation batch и общий byte limit. Один слишком длинный сегмент имеет собственный text continuation. Проверять UTF-8 границы и сообщать truncated/next_cursor.

| Ошибка | Значение |
| --- | --- |
| AUTH_REQUIRED / ACCESS_REVOKED | Нет действующего подключения |
| SERVICE_STARTING / SERVICE_UNAVAILABLE | Service ещё запускается либо недоступен |
| LIBRARY_UNAVAILABLE / UPGRADE_REQUIRED | Библиотека отсутствует или несовместимы binary/API/schema |
| INVALID_ARGUMENT / NOT_FOUND | Неверный input либо отсутствующий объект |
| REVISION_CONFLICT / CURSOR_STALE | Данные изменились; требуется перечитать |
| IDEMPOTENCY_CONFLICT | Key уже использован для другого запроса |
| RESOURCE_BUSY / RATE_LIMITED | Нельзя выполнить сейчас; причина и retry policy |
| SEARCH_UNAVAILABLE / INDEX_BUILDING | Нет достоверного полного поискового ответа |
| SHARE_PREPARATION_FAILED / BACKEND_UNAVAILABLE | Подготовка публикации/исполнитель недоступны |
| JOB_INTERRUPTED / RESYNC_REQUIRED | Interrupted operation либо change cursor устарел |

## 5. Поиск по четырём источникам

Первый обязательный результат — полноценный lexical FTS5 без embedding-модели. Семантическое улучшение не блокирует поиск и не запускает LLM как побочный эффект.

### Индекс

- search_documents: document ID, kind=title/transcript/summary/note, meeting_id nullable для самостоятельной note, source_id/revision, representation, searchable body, optional segment/times.
- Один title document на встречу, один transcript document на сегмент, один документ на current summary и на note.
- Effective transcript — COALESCE(clean_text,text); намеренная пустая строка не подменяется raw.
- При default effective search прежнее raw-слово не должно находиться только потому, что оно сохранилось в оригинале. Raw/both — отдельный явный режим с согласованными query/excerpt/citation.
- Для current transcript индексировать raw/effective отдельно; при both одинаковые body объединяются с явной representation. Archives не входят в текущий поиск; current source ID является частью citation.
- Current summary и own notes входят в индекс; AI live notes не индексируются под видом пользовательских.
- Индекс обновляется в той же транзакции, что source mutation. Триггеры проекции допустимы, но не заменяют revisions/policy.
- Удаление summary/note/meeting сразу удаляет searchable projection, включая старые raw/effective представления, если они больше не разрешены выбранным режимом.

Для первой миграции измерить atomic backfill на synthetic large library. Если он слишком долог, реализовать restartable generation build с INDEX_BUILDING и проверкой полноты; не отдавать partial index как полный. Старые FTS/chunks временно сохраняются для существующего retrieval, затем их использование сводится к одному контракту.

### Результат и страницы

Каждый hit содержит field/kind, meeting_id или note_id, source_id, source_revision, title/date, excerpt, source_uri и optional start_ms/end_ms. У summary/note нет выдуманного таймкода. Chunk IDs не выдаются как постоянные citation IDs.

Cursor связан с library ID, normalized query, filters, representation, сортировкой и search_generation. При изменении generation между страницами первый выпуск возвращает CURSOR_STALE. Между вызовами не удерживается SQLite transaction. Внутри одного ответа source и revision читаются согласованно.

Ранжирование: BM25 с документированными полями/весами, stable tie-breaker. Фильтры — период, meeting IDs как query filter без ACL, folder/tag, kind и participants. Даты — ISO 8601 с timezone, времена сегментов — integer ms.

Hybrid позже переиспользует существующий retrieval после выделения typed diagnostics. Отдельно показывать lexical readiness, semantic coverage и фактически использованный mode. Default English embedding не считается доказательством русского semantic quality.

## 6. Sharing и lifecycle фонового сервиса

### Публикация

Сервис владеет LAN listener; окно/adapter не владеют его временем жизни. Не открывать listener при обычном чтении библиотеки без активных shares. LAN HTTP sharing и private control IPC остаются разными границами.

Существующие shares мигрируют как live, сохраняя их поведение. Для новых MCP shares по умолчанию snapshot: конкретная версия выбранных материалов; live задаётся явно. Состав публикации явный: transcript representation, summary, выбранные notes и media. Собственные заметки по умолчанию исключены. Разрешённый share tool исполняется без per-meeting UI approval.

Public share_id отделён от bearer token. URL возвращается create/get/list только при явном include_url; tokens отсутствуют в логах и audit. Состояния: preparing/active/disabled/failed/revoked/expired.

Clip или snapshot сначала создаётся preparing и не обслуживается публично. Файлы рендерятся в staging, затем service проверяет source revisions, grant и tombstone и активирует результат. Изменившийся источник не публикуется незаметно под прежним snapshot.

Update_share поддерживает enabled/expiry. Изменение состава или режима создаёт новую publication revision; при расширении доступа mint нового token и отзыв старого входят в одну согласованную operation. Delete/revoke сразу прекращает новые page/media requests; cleanup snapshots/clips отслеживается job. Уже отправленные получателю bytes невозможно отозвать задним числом.

Share live page и media handler проверяют enabled/expiry/tombstone при каждом новом запросе; snapshot не должен обходить отзыв исходной встречи. Notes, появившиеся позже, не включаются в live share без изменения опубликованной field policy.

Publication dependencies связывают snapshot с source IDs/revisions. Permanent delete note/summary/transcript транзакционно отзывает зависимые managed snapshots/shares и ставит их files в purge; они не продолжают отдавать удалённый документ. Обычное редактирование сохраняет прежний snapshot до явного update/revoke. Meeting delete отзывает все его публикации.

### Service lifecycle

Удержание сервиса: enabled shares, running/queued executable jobs, connected GUI/MCP, активные native worker leases или включённый background calendar. Само наличие cached events не заставляет daemon работать постоянно. Если включена фоновая работа после logout/reboot, пользователь выбирает login startup отдельно; это не обязательный Windows system service.

При service restart активные shares восстанавливаются из БД, но доступность listener показывается отдельно от saved enabled state. Port bind failure — явный health/error, а не успешный рабочий URL. На Windows GUI/agent process-tree termination не должен убивать service.

### Полученные общие встречи

Для full shared-source этапа определить versioned read-only JSON manifest/content endpoint поверх существующего share token: опубликованные поля, source revision, media policy, expiry. HTML scraping не использовать.

Catalog хранит source ID, защищённую credential reference, remote identity/revision, allowed fields и freshness. Refresh только настроенных совместимых источников; network timeouts/size limits, redirect/URL policy и явный offline/stale status обязательны. Встреча может быть прочитана по cache либо импортирована локально с origin metadata.

В локальный search включать cached shared content только в пределах выбранной local cache policy. При полученном отзыве/expiry исключать его из выдачи. Offline cache не доказывает сохранение текущего remote permission; отображать это состояние, не обещать real-time remote revocation в офлайне.

CRUD чужой встречи ограничен исходными правами; создание локальной копии не изменяет оригинал у владельца. Добавление remote sources — отдельное явное действие настройки/команды, а не универсальный URL fetch из текста встречи.

## 7. План работ и критерии завершения

Галочки отражают реализацию и actual acceptance. Частичная проверка не закрывает пункт целиком. Текущие доказательства и ограничения: [отчёт этапа 0](../test-reports/2026-11-02-mcp-phase-0.md).

### Этап 0. Контракт, аудит writers и baseline

Файлы: root/scoped AGENTS; state/lib/commands/recorder/pipeline/stt/summary/video_diagnostic; store/share; текущий testing guide.

- [x] Зафиксировать M1–M11, tool schemas, library-grant semantics и границу internal/external RPC.
- [x] Составить список всех Store constructors, DB mutations и managed file writes; отличить atomics .store() от persistence.
- [x] Зафиксировать lifecycle запуска/Exit/close-to-tray и конкретный протокол старта service.
- [x] Подготовить synthetic v8 library: записанная/повреждённая встреча, raw/manual/empty overlays, summary origins, chats, shares/clips, voice samples, RU/EN.
- [x] Снять доступный Windows software baseline на искусственных материалах: recording state/admission/defaults, provider consent revoke, recovery, correction, import/subtitles, Player state, shares и summary-runner isolation.
- [x] Проверить Windows demand-start broker из текущего desktop execution host: kill launcher tree не уничтожает service; concurrent clients подключаются к одному owner.
- [ ] Выполнить отдельно macOS/реальные Codex и Claude Code MCP host acceptance; hardware capture/waveform и decoder/asset-protocol A/V остаются отдельными native gates перед production cutover.
- [x] Если ограничения job/process group хоста не позволяют breakaway, выбрать поддерживаемый user-session launcher/broker при первоначальной локальной настройке. Не считать обычный child spawn доказательством независимого lifecycle.
- [x] Проверить lean dependency graph service/client skeleton: нет Tauri/whisper/llama/sherpa/capture; повторить для CRUD на этапе 2 и adapter на этапе 7.
- [x] Выделить pure builtin templates/defaults из native LLM initialization либо встроить их как service resources; seeding не должен возвращать heavy inference dependency.

Готовы [writers/lifecycle source map](../research/2026-11-02-mcp-writer-audit.md),
[29 executable schemas и граница полномочий](../research/2026-11-02-mcp-contract-boundary.md),
frozen v8 fixture и upgrade/reopen tests. V8 summary origins представлены synthetic
markers; typed persistent origins появятся на этапе 2. Семь суток dedup — закреплённый
целевой контракт, не уже реализованная гарантия Store.
[Windows baseline](../test-reports/2026-11-02-mcp-windows-baseline.md) и
[результаты этапа 1](../test-reports/2026-11-02-mcp-phase-1.md) фиксируют software/native
tests и явные ограничения. Windows host отверг direct breakaway; выбранный
InteractiveToken/Limited Task Scheduler broker проверен с concurrent starters,
kill launcher tree и restart на пути с пробелами/кириллицей. Временные задачи удалены.
Пользователь уточнил очередность: доступный Windows scope, затем этап 1; отложенные
macOS/hardware/real-host gates не подменены mocks и не объявлены выполненными.

Gate: есть воспроизводимые fixtures, source map и решение по detached lifecycle; capture behavior/defaults зафиксированы. Реальные пользовательские встречи не используются.

### Этап 1. Service skeleton, identity и совместимость

Файлы: новые service-api/client/service/core; Cargo manifests; bazza-store/db.rs и migrations.rs.

- [x] Определить versioned typed private RPC: correlation/request IDs, OS-user auth context, deadlines, bounded frames/queues и безопасные errors.
- [x] Реализовать per-user endpoint и canonical-library ownership guard; owned Store удерживает guard до закрытия Connection. Legacy GUI cutover остаётся этапом 3.
- [x] Разделить open-existing, create-library и migrate. Ошибочный путь не создаёт пустую библиотеку.
- [x] До migrations/recovery отвергать schema выше поддерживаемой, включая effective committed WAL; migrations::run также проверяет версию.
- [x] Handshake проверяет API major/minor, schema range, API-source build identity, library ID/epoch и сообщает фактические capabilities.
- [x] Сделать атомарный endpoint discovery и OS peer verification; stale PID/epoch и mismatches отвергаются, public connection ID не является аутентификацией.
- [x] Реализовать authenticated private status/meeting metadata read, graceful shutdown/restart и запрет второго мигрирующего service.
- [x] На disposable library проверить три private-client stand-ins и concurrent Windows broker launch: один owner, stable ID/new epoch после restart.
- [ ] Настоящий GUI + два публичных MCP adapters проверяются после их cutover/реализации на этапах 3/7/10; текущие stand-ins не заменяют эту приёмку.

Windows skeleton работает только с явно помеченными synthetic fixtures. В service
нет production default library, capture/inference/calendar/sharing startup; public
CRUD, external grants и MCP handlers ещё не реализованы. Private OS-user application
authentication не заменяет external principal/library grant. Migrations 1–9 сохранены;
GUI по-прежнему использует legacy Store constructor, поэтому production enable до
version interlock и zero-write gate этапа 3 запрещён.

Gate: один Store owner; второй launch подключается к нему; mismatch даёт контролируемый отказ без SQLite fallback.

### Этап 2. Schema и общее data core

Файлы: bazza-store/migrations.rs, meetings.rs, segments.rs, ai.rs, organize.rs, новые notes/revisions/jobs/change modules; bazza-core.

- [x] Добавить append migrations из раздела 3 и baseline revisions; проверить пустую и v8 library.
- [x] Ввести targeted transactional mutations, expected revisions, actor provenance, change sequence и dedup outcomes.
- [x] Сохранить state generations при absent/deleted summary/transcript; explicit summary delete подавляет automatic recreation.
- [x] Добавить immutable transcript sources и transactional import/replace/delete без изменения исходного raw при overlays.
- [x] Реализовать notes CRUD, summary current/history policy и calendar cache repositories.
- [x] Добавить content_kind=text, media availability и валидаторы создания готовой встречи.
- [x] Batch corrections проверяет все сегменты, сохраняет raw/manual/empty/reset и один раз rebuild chunks.
- [x] Добавить jobs registry и durable staging/deletion state machine.
- [x] Создать unified search projection и backfill/readiness; синхронные source updates обеспечивают consistency.
- [x] Проверить rollback на ошибке миграции и повторный запуск; исходные поля/media hashes сохранены.

Gate: domain API проходит meaningful store/core tests без Tauri, capture и моделей.

Gate пройден только для service-domain API на disposable fixtures:
[отчёт этапа 2](../test-reports/2026-11-02-mcp-phase-2.md). Legacy GUI writers
не подключены к новым consistency hooks; public RPC/MCP CRUD, dispatch/jobs
keepalive и managed snapshot publisher не включены. Domain DTO/paging не заменяет
полные public projections этапов 4–5; internal worker artifacts не являются
share snapshots этапа 6.

### Этап 3. Перевод GUI и workers на service

Это самый большой интеграционный этап. До его завершения MCP writes в реальную рабочую библиотеку не включаются.

Первый increment проверен отдельно: [worker/capture handoff](../test-reports/2026-11-02-mcp-phase-3.md).
Append v11, registered PID/token-bound workers, OS artifact guard, confirmed journal,
terminal seal, atomic finalize/receipt/job и typed private RPC работают на fixtures.
Обычный GUI/recorder не переключён, поэтому пункты полного cutover ниже остаются
открыты. До подключения нужны также abort для unsealed prepare, large-handoff
protocol и cloud permission/cancel integration; heartbeat не заменяет artifact lock.

Второй increment добавляет append v12, настоящий cloneable RepositoryClient и
семь bounded private desktop RPC: bootstrap/preferences/patch, meeting page/header,
change page и rename. Каждый результат содержит library/epoch/scope; cursors
связаны с identity/query/sequence, mutations требуют исходную revision и key.
Preferences ограничены locale/close-to-tray/hotkey; capture/STT settings не
переписываются. Rename сохраняет capture fields и допустим во время записи.
UI допускает audio=null и скрывает недоступные media actions; проверен отдельный
text-only mock. Native get_meeting и AppState ещё не переведены. Это prerequisite,
а не завершённый пункт optional-media/native cutover ниже. Подробности и реальные
fixture process tests — в том же отчёте этапа 3.

Третий increment добавляет private transcript/summary fragment pages и точную
транзакционную очистку legacy/versioned meeting voice caches при replace/delete.
Current/archive reads используют immutable source, не mutable segments projection;
manual-empty и UTF-8/JSON budgets проверяются отдельно. Native get_meeting теперь
допускает отсутствие audio и не создаёт recordings directory при чтении.
Отдельный `--service-fixture` GUI запускается до Store/AppState startup,
с отдельным identifier и bounded panel. Schema21 добавляет явную запись с
микрофона в этом окне. Обычные routes и production writers ещё предстоит
перевести на сервис.

| Текущий участок | Изменение |
| --- | --- |
| lib.rs/state.rs | Убрать Store::open/Arc<Store>, migration/share/calendar startup; подключить RepositoryClient |
| commands/meetings, organize, ai, speakers | Вызов общих services вместо прямых writes; сохранить typed UI contract |
| settings, ui_locale, asr_pick | Persistence settings/locale/measured speed также через service |
| recorder/recovery | Prepare/finalize capture и recovery claims через worker protocol |
| pipeline/embed/summary/stt stages | Input snapshot и fenced result commits; engines/исполнение пока в GUI |
| commands/ai chat | Bounded chat/message pages, revision CAS и fenced completion; citation erase не должен восстанавливаться поздним ответом |
| video_diagnostic | Sidecar update и DB projection — единая service-owned операция |
| import/bundle/export | Staging, metadata commit и file publication под контролем service |
| managed voice samples/models | Repair/download/remove через service либо ограниченного worker; loaded engine получает read lease |
| smoke/test harness | Temporary library + service client; не отдельный production DB opener |

- [ ] Ввести capture lease: library epoch, meeting generation, worker session, purpose и fencing token.
- [ ] GUI raw writer держит OS-level artifact lock до закрытия/join/finalize; expiry heartbeat сам по себе не разрешает orphan recovery.
- [ ] Service restart допускает reattach живой capture session; recovery начинается после подтверждения отсутствия живого artifact owner.
- [ ] Capture callbacks остаются bounded и не делают RPC. Во время service outage уже начатый local writer сохраняет raw artifacts.
- [ ] После close/join/fsync capture worker сохраняет versioned terminal manifest и durable confirmed-STT journal с sequence IDs. Идемпотентный finalize_capture применяет ещё не принятые captions/provenance и одной DB transaction фиксирует final state, durable post-process job и outcome; GUI получает durable ACK.
- [ ] При потерянном finalize ACK повтор безопасен; при смерти GUI service после освобождения artifact lock обнаруживает pending handoff и регистрирует job без запуска inference. Не терять selected-provider provenance, не дублировать принятые captions и не заменять Soniox на Whisper неявно.
- [ ] Разделить Soniox permission epoch и registered worker. Revoke/key clear/provider change — coordinated cancel/ack; потеря control channel останавливает cloud upload, local writer продолжает.
- [ ] Pipeline jobs фиксируют checkpoints/source revisions; staging artifacts публикует service. Поздний worker с устаревшей generation не создаёт файлы/rows заново.
- [ ] Jobs имеют executor_kind, required_capabilities, claim/ack/lease/reclaim policy и waiting_for_worker. GUI Exit сохраняет checkpoint и переводит GUI job в interrupted/waiting; новый GUI принимает его без двойного commit. Service не обещает inference без подходящего worker.
- [ ] Не использовать MeetingStatus::Ready как «все jobs завершены»; существующий pipeline выставляет его раньше cleanup/summary/webhook.
- [ ] Реализовать change feed с seq; reconnect/refetch при gap; RESYNC_REQUIRED при устаревшем cursor.
- [ ] Сохранить busy_guard у GUI engines, current close-to-tray и capture Exit/recovery policy. True Exit из idle не закрывает service.
- [ ] До включения text-only writes перевести минимальные get_meeting/Media.audio/Player на optional media, отключить media-only actions для text встреч; старый typed UI contract расширить согласованно.
- [ ] Перенести Change data folder в service-only relocation: quiesce новых операций, drain leases/jobs/share streams/model readers, consistent snapshot+copy+verify, atomic pointer/manifest switch, reopen с новым endpoint/epoch, reconnect clients. При ошибке source остаётся активным. Старую GUI copy logic удалить; до готовности протокола команда явно недоступна.

Zero-write gate: в production src-tauri нет Store constructors/connections/Arc<Store>; managed library writes выполняет service или его fenced worker. GUI outage не запускает локальный DB fallback. Тест GUI+два adapters подтверждает это по всем commits.

Первый cutover требует остановленного старого GUI и согласованного snapshot. Старый executable не знает новых advisory locks; один lock-файл не защищает от запуска legacy writer. Для согласованного install нужна version interlock; запуск произвольного старого бинарника против новой БД не считается поддержанным.

### Этап 4. Завершение CRUD и удаления

Файлы: core services; store repositories; service job/file executor; meeting/note/summary UI wrappers.

- [ ] Create text meeting без WAV, с title/date/participants и optional готовым timed transcript.
- [ ] Update metadata, folders/tags/calendar links — typed patch, revision, без full-row lost update.
- [ ] Notes CRUD с самостоятельными и привязанными документами; summary create/replace/delete с history/provenance.
- [ ] Transcript import/replace/delete обновляет persistent state, archives/current projection, provider rows, citations и индекс; explicit delete не воссоздаётся поздним worker.
- [ ] Corrections — atomic batch и понятный saved/index_pending outcome.
- [ ] Delete meeting: active capture даёт RESOURCE_BUSY; никаких скрытых Stop.
- [ ] В одной transaction выставить tombstone/generation, убрать search, disable shares, записать cancel intents и purge operation.
- [ ] Вне DB lock дождаться освобождения workers/readers, выполнить staging/purge managed artifacts; Windows open handles дают retryable purge_pending.
- [ ] Cleanup охватывает media, clips/snapshots, segments/chunks/vectors, summary/history, linked notes, meeting chats/messages и scoped settings stt/stt_previous/highlights/speaker_embeddings.
- [ ] Global voice memory не удалять неявно; определить null/tombstone source reference для samples удалённой встречи.
- [ ] У сохранённых global samples очистить source meeting link/title label и сообщить retained_count; не считать удалённую source identity доказательством независимых встреч для voice matching.
- [ ] Permanent delete отдельного note/summary/transcript отзывает зависимые snapshot publications и запускает их purge.
- [ ] Повтор delete/purge безопасен; late summary/diagnostic/worker commit отклоняется.

Gate: без GUI полный create→read→update→delete работает; объект сразу недоступен после logical delete, физическая очистка имеет наблюдаемый terminal outcome.

Delete response не сообщает «все файлы удалены», пока purge job не succeeded. DB и filesystem не объявляются одной атомарной транзакцией. Purge manifest строится сервисом из canonical IDs; пути, junctions/symlinks и containment проверяются.

### Этап 5. Полный поиск и citations

Файлы: store search module, core query services, embed_stage adapter, API DTO, UI search/Ask.

- [ ] Искать по каждому из четырёх fields отдельно и по всем вместе.
- [ ] Обрабатывать empty cleaned overlays и корректно отделять raw/both.
- [ ] Реализовать SQL pagination списка встреч и bounded transcript/note/summary reads.
- [ ] Добавить result source_kind/revision/URI/times и CURSOR_STALE на изменение search generation.
- [ ] Все writers, включая imports и background stages, синхронно обновляют projection/change state.
- [ ] Отличать no_matches, lexical fallback, partial semantic coverage, INDEX_BUILDING и SEARCH_UNAVAILABLE.
- [ ] Подготовить RU/EN fixtures: имена, даты, сокращения, кириллица, перефразирование и совпадения только в note/summary.
- [ ] Измерить large synthetic library latency, DB lock hold и memory; установить budgets по baseline, без выдуманных обещаний.

Gate: после любого CRUD новая выдача соответствует committed данным; исправленный/удалённый текст не выдаётся как текущий; citations разрешаются обратно в источник.

### Этап 6. Headless calendar и sharing

Файлы: bazza-service executors; bazza-calendar adapter; bazza-share/server.rs/html.rs/staging.rs; store share models.

- [ ] Перенести calendar refresh в service; сохранить offline cache и per-source freshness/errors.
- [ ] Перенести ShareServer handle из AppState; listener восстанавливается при service start.
- [ ] Добавить snapshot/live field policies и сохранить поведение старых live shares.
- [ ] Подготовка clip/snapshot — durable job и staging; active только после успешной проверки/публикации.
- [ ] Добавить share expiry, status, creator grant, revisions, update/revoke semantics.
- [ ] При закрытом GUI проверить HTML, transcript/summary/выбранные notes, audio/video range playback и seek.
- [ ] Revoke/delete meeting/share блокирует новые запросы, отменяет preparing jobs и очищает artifacts.
- [ ] GUI Exit и EOF adapter не прекращают активный LAN listener; service idle policy соответствует разделу 6.
- [ ] После service restart восстановить publication state; port conflict отображается как unavailable.

Gate: create/update/revoke share и calendar refresh выполняются без GUI; активная ссылка работает после завершения GUI и AI-клиента, пока работает независимый service.

### Этап 7. MCP adapter и library authorization

Файлы: bazza-mcp, service-api/client/auth, local admin CLI.

- [ ] Закрепить проверенный RMCP release и lockfile; реализовать tools из раздела 4 поверх общих services.
- [ ] Проверить modern 2026-07-28 и заявленную legacy 2025-11-25; lifecycle/serialization отдаёт SDK.
- [ ] Поддержать structuredContent и совместимый TextContent; outputSchema допускает error envelope.
- [ ] Names/descriptions/input bounds/annotations отражают фактические side effects. Tools/list имеет стабильный порядок.
- [ ] Реализовать pairing, whole-library grant, secure credential lookup, list/revoke admin без GUI.
- [ ] Не требовать открытия встречи или UI подтверждения для уже разрешённых CRUD/share.
- [ ] Проверять revoked grant на работающем соединении и перед deferred commits/publication.
- [ ] Проверить race revoke между preflight и commit/activation; mandatory cleanup committed mutations отзыва не боится.
- [ ] Ограничить cancel_job по public job kinds/cancellable; известный capture/purge ID не позволяет обойти recording/delete lifecycle.
- [ ] stdout содержит только MCP; diagnostics в stderr без текстов встреч/секретов.
- [ ] EOF завершает adapter быстро; service jobs не отменяются вместе с транспортом.
- [ ] Добавить optional resources через ту же authorization/domain API; private cache policy, bounded content.

Gate: Inspector и два реальных клиента выполняют base flows по stdio; право на всю библиотеку работает без per-meeting approval от сервера.

[Fixture-адаптер](../../../crates/bazza-mcp/README.md) использует закреплённый
RMCP 3.8.0 и оба протокола. Девять инструментов работают на синтетической
библиотеке. Для режима с правами есть отдельный IPC; Windows pairing, поиск
секрета в Credential Manager и отзыв прошли сквозную проверку. Авторизация
рабочей библиотеки, macOS Keychain и полный каталог остаются частью этапа 7.

### Этап 8. GUI, text-only и переносимые bundles

Файлы: app/src/lib/ipc.ts/mock.ts; Settings/Meeting/Library/Player/ShareDialog/notes UI; ru/en locales; bazza-share/bundle.rs; store import.

- [ ] Settings показывает library grant, операции, active connections, recent action metadata, health и revoke.
- [ ] Сгенерировать конфигурацию абсолютного executable + public connection ID; проверить тот же credential lookup, что у host.
- [ ] Добавить notes CRUD UI; показывать автора/источник и конфликт версии.
- [ ] Media.audio в UI DTO становится optional; text-only meeting не создаёт recording directory при чтении.
- [ ] Player, exports, talk-time, empty transcript и sharing корректно работают без audio.
- [ ] MCP mutations обновляют все открытые окна через invalidation/refetch; устаревший response не затирает новую UI-правку.
- [ ] .bazza reader принимает v1 и v2; новый writer v2 переносит content_kind, notes, current summary/provenance и optional media.
- [ ] V2 manifest содержит top-level standalone_notes для явно выбранного library/note export. Export выбранных встреч включает их linked notes; самостоятельные заметки не присоединяются к вымышленной встрече.
- [ ] Import standalone notes сохраняет content/origin, назначает local revisions и применяет документированный duplicate-ID skip; note collision не становится silent overwrite.
- [ ] По умолчанию bundle переносит current transcript source с raw/overlays. Archived sources/summary history — только явный include_history с manifest flag и отдельными size limits; current-only export не обещает перенос истории.
- [ ] V1 остаётся recorded с legacy defaults; v2 text-only допускает media=[]; настоящий recorded media validation сохраняется.
- [ ] Import создаёт новые local revisions/index; existing meeting ID по-прежнему skip, без неявного merge.
- [ ] В bundle не попадают grants, tokens, secrets, change/idempotency logs, machine paths и global voice memory. Source citations импортируются как origin refs, не действующие локальные segment IDs.
- [ ] Сохранить media allowlist, path/size/entry limits, staging/retry и manual empty overlays.

Gate: v1 import и v2 recorded/text-only/standalone-note roundtrip сохраняют заявленный состав, raw/manual/provenance/media hashes; include_history проверяется отдельно; браузерный mock не считается native proof.

### Этап 9. Полученные shared meetings

Этот этап закрывает исходное требование о встречах, которыми поделились другие без локального bundle import; он не блокирует собственные shares.

- [ ] Зафиксировать versioned read-only manifest/content API для совместимых Bazza shares.
- [ ] Добавить registered sources, credential lookup, access/freshness state и bounded cache.
- [ ] Реализовать list/get/refresh tools для источников, а не generic HTTP fetch.
- [ ] Добавить явный import-copy с origin metadata; редактура локальной копии не меняет владельца.
- [ ] Проверить revoked/expired source, offline cache, changed content, ограниченный field policy и отсутствие media/notes сверх выданных прав.
- [ ] Определить cache retention и search inclusion; отображать stale/unknown remote permission.

Gate: две synthetic libraries обмениваются совместимым read-only share; получатель видит только опубликованные данные и честное состояние доступа.

### Этап 10. Упаковка, совместимость и итоговая приёмка

Файлы: Cargo manifests/build metadata; scripts/prepare-bundle.mjs; stage-runtime-libs.mjs; src-tauri/build.rs/tauri.conf.json; capabilities/entitlements при необходимости; testing/privacy/import/architecture docs.

- [ ] Локально собирать GUI/service/adapter согласованного build; учитывать profile, target triple и CARGO_TARGET_DIR.
- [ ] Adapter запускается по абсолютному пути без cwd/PATH зависимости; service path определяется проверенным install manifest.
- [ ] Service detached от Job Object/process group GUI/agent; Windows helper без видимого console window.
- [ ] Обновление quiesces сервис, освобождает ownership и затем запускает совместимый binary; mixed versions fail closed.
- [ ] Rollback делается согласованным snapshot+binary при остановленных владельцах; не понижением user_version.
- [ ] Проверить packaged startup на Windows и macOS, включая space/non-ASCII paths и redirected data-dir.
- [ ] Проверить lean CRUD без AI DLL/dylib/model prerequisites; native workers требуют свои штатные зависимости.
- [ ] Пройти матрицу раздела 8; зафиксировать конкретные binary hashes/client versions/commands и ограничения.
- [ ] Обновить фактическую architecture/privacy/testing/import документацию при внедрении новой границы. AGENTS меняются только если старая граница реально заменена.

Gate: M1–M11 подтверждены actual local evidence; нет открытого критического риска потери данных/полномочий. Публичной публикации в этом этапе нет.

## 8. Матрица обязательной приёмки

| Сценарий | Проверяемый результат | Требования |
| --- | --- | --- |
| GUI PID отсутствует | Create/read/update/delete note/meeting/summary и search работают | M1–M6 |
| Новый meeting после выдачи grant | Доступен без отдельного согласования | M1 |
| GUI и два агента | Один Store owner, conflict вместо lost update, все окна актуальны | M8 |
| Потерянный ответ mutation | Retry того же key не применяет повторно | M10 |
| Смена текста между transcript/search pages | Явный revision/cursor conflict, без смешения версий | M6, M8 |
| Все writers | Title/raw/effective/summary/note/import/background changes согласованы с FTS и revisions | M6, M8–M9 |
| Ручная пустая правка | Raw сохранён, effective пуст, старый raw не найден default search | M6, M9 |
| Stale summary/embedding worker | Новый текст/итог не затёрт, vectors не привязаны к старому source; create→delete не сбрасывает generation | M8–M10 |
| Summary delete→rename/reprocess/restart | Итог остаётся отсутствующим до explicit write/regenerate | M5, M10 |
| Transcript import/replace/delete | Raw archives сохранены при replace, очищены при explicit delete; provider copies/индексы и citations согласованы | M3, M9–M10 |
| Delete note/summary/transcript со snapshot | Зависимый старый URL недоступен; snapshot files очищаются | M3–M7, M10 |
| Delete при открытом media/late worker | Tombstone и revoke сразу; purge retry; нет resurrection | M3, M7, M10 |
| Active capture delete | RESOURCE_BUSY; локальная запись не останавливается неявно | M3, M9 |
| Service crash при живом capture | Reattach или guarded recovery, без второго ffmpeg/recovery | M8–M10 |
| Kill до/после finalize ACK | Media и confirmed captions сохранены, успешная запись создаёт одну durable job, failed capture завершается без job, повтор finalize безопасен | M8–M10 |
| GUI Exit при inference job | Честный waiting_for_worker, data CRUD продолжается; новый worker не делает двойной commit | M2, M8–M10 |
| Известный ID capture/purge в cancel_job | Отмена запрещена; capture и обязательная очистка продолжают штатный lifecycle | M3, M8–M10 |
| Consent/key revoke и IPC outage | Cloud upload остановлен по control policy; local media продолжается | M9 |
| GUI/agent process tree killed | Service и share/job lifecycle независимы | M2, M7, M10 |
| Share restart/update/expiry/revoke | Верные поля, source snapshot, доступность и отказ новым requests | M7 |
| Grant revoked при queued/running/preparing | Нет новых unauthorized commits/publication; runtime не требует GUI | M1, M10 |
| Calendar offline/partial failure | Cache переживает restart; stale/error не выдается за empty | M2 |
| V8 migration / V1/V2 bundle | Исходные данные сохранены; linked/standalone notes и text-only portable; secrets отсутствуют | M4, M9 |
| Old/new binaries | Контролируемый mismatch; второго Store owner нет | M8–M11 |
| Relocation при активных worker/stream | Quiesce либо явный busy; нет записи в старый root после epoch switch | M8–M11 |
| Malicious transcript/title/note | Содержимое не меняет permissions/API/recipient, hidden generic tools отсутствуют | M1, M10 |
| Windows и macOS реальные hosts | Отдельно stdio lifecycle, credentials, service start, all base workflows | M11 |

Команды при реализации, после появления новых пакетов:

~~~powershell
cargo test -p bazza-store -p bazza-core -p bazza-service-api
cargo test -p bazza-service -p bazza-service-client -p bazza-mcp
cargo test -p bazza-share -p bazza-calendar
pnpm typecheck
pnpm test
node scripts/check-docs.mjs
git diff --check
~~~

Сначала affected packages и meaningful synthetic integration tests. Native desktop compile/build и recording baseline выполняются по testing.md с реальными sidecars; документация сама по себе их не требует. Не использовать BAZZA_SMOKE как безвредный data test — он записывает аудио.

MCP Inspector проверяет discovery/tools/resources/stdout, а frozen conformance — применимые требования конкретных protocol revisions. Это не заменяет product acceptance. Фактические версии Inspector/SDK фиксируются на этапе 7. [Inspector](https://modelcontextprotocol.io/docs/2026-07-28/tools/inspector), [Conformance framework](https://github.com/modelcontextprotocol/conformance).

Agent eval: held-out RU/EN tasks на synthetic library; правильные источники/цитаты, нужные calls, no-results/degraded behavior, tokens/latency. Особенно проверить запросы, ответы на которые находятся только в summary или собственных notes.

## 9. Зависимости и параллельная работа

Критический путь: контракт/baseline → singleton service/schema → GUI/worker cutover → CRUD/search/share integration → packaged native acceptance.

| Поток | Можно делать параллельно после контракта | Общая граница / gate |
| --- | --- | --- |
| Storage/core | Migrations, notes/summary, revisions, search, deletion | Один владелец схемы и transactional helpers |
| Host/client | IPC, locks/bootstrap, credentials, lifetime, build handshake | Один владелец RPC и DTO |
| GUI/workers | RepositoryClient, leases, invalidation, optional media/notes UI | До production нужен zero-direct-write gate |
| MCP | Tool handlers/schema tests на fake service, descriptions, Inspector harness | Подключение реальных writes после service/core/cutover gates |
| Sharing/calendar | Headless lifecycle, cache, publication jobs | Разрешения и shared job/file primitives из core |

Общие DTO, migrations и lifetime contracts не редактировать независимо несколькими исполнителями. Выделять узкие файлы/участки, интеграцию проверяет основной исполнитель. Сначала вертикальный срез на disposable library, затем широкая миграция GUI writers.

Точные календарные сроки до spike не назначаются. Наибольшая стоимость — этап 3 и native regression; тонкий MCP adapter проще ownership/capture boundary. После этапов 0–1 оценить оставшиеся work packages по фактическому объёму, отдельно заложить Windows/macOS приёмку.

## 10. Решения и риски для следующего исполнителя

| Риск | Принятое направление |
| --- | --- |
| GUI остаётся вторым неконтролируемым writer | Полный RepositoryClient cutover, запрет Store constructors и production fallback |
| Старый binary игнорирует новый lock | Stopped-owner cutover, version interlock и неподдерживаемость legacy manual launch |
| Второй recovery при живом capture | Fencing + OS artifact ownership; одного heartbeat недостаточно |
| Native child пишет после выхода guard owner | Гарантированное child lifetime binding при spawn до raw writes; нынешний best-effort Windows job и отсутствие macOS binding не закрывают gate |
| Потеря мгновенного cloud revoke после разделения процессов | Permission epoch, registered session и cancel/ack; outage fail-closed для upload |
| Поздние diagnostics/summary создают удалённые файлы | Staging publication через service, tombstone/generation gate |
| «Удалено» при оставшихся файлах | Durable purge job и отдельные logical/physical outcomes |
| Поиск выдаёт прежний raw как effective | Отдельная проекция, согласованные query/snippet/citation |
| Нет notes или сохранённого calendar cache | Новые сущности и явная миграция, не переименование live notes |
| Text-only ломает player/bundle | Optional media + v2 writer / v1+v2 reader в release boundary |
| Share link пропадает вместе с клиентом | Detached service и active-share keepalive |
| Автогенерация затирает внешний summary | Typed origin, source/target revisions и auto_replace policy |
| Неполный индекс объявлен полным | Index generation/readiness и fault-tested backfill |

## 11. Источники и фактологическая основа

Актуальность проверена 2 ноября 2026 года: стабильная MCP revision 2026-07-28; официальный Rust SDK release list показывает rmcp-v3.8.0. Целевой SDK закрепляется после compile/compatibility проверки; в план не закладывается ручная реализация version negotiation. [MCP specification](https://modelcontextprotocol.io/specification/2026-07-28), [RMCP releases](https://github.com/modelcontextprotocol/rust-sdk/releases).

Codex поддерживает stdio servers с command/args; конкретная версия и поверхность проверяются при приёмке. Claude Code проверяется отдельно, включая protocol runtime. [Официальная документация OpenAI](https://learn.chatgpt.com/docs/extend/mcp?surface=cli), [Claude Code MCP](https://code.claude.com/docs/en/mcp).

Source seams: [Store opening](../../../crates/bazza-store/src/db.rs), [migrations](../../../crates/bazza-store/src/migrations.rs), [segments/search](../../../crates/bazza-store/src/segments.rs), [summary/chunks](../../../crates/bazza-store/src/ai.rs), [AppState](../../../src-tauri/src/state.rs), [startup](../../../src-tauri/src/lib.rs), [STT control](../../../src-tauri/src/stt_stage.rs), [diagnostics](../../../src-tauri/src/video_diagnostic.rs), [sharing/bundles](../../../crates/bazza-share/src/lib.rs).

## 12. Проверки и передача

Этапы 0–2 реализованы и проверены в доступном Windows scope. Этап 3 остаётся
активным. Ordinary GUI/recorder/pipeline ещё используют Store. Закрытый
read-only stdio adapter работает на синтетической библиотеке. Production
mutations и grants требуют zero-direct-write/native regression gates.
Пользовательская библиотека и normal GUI не используются для synthetic acceptance.

Текущие реализованные private границы:

- Owner/version interlock, ordinary open_legacy до v9, startup после single-instance
  admission и до webviews/IPC; explicit service upgrade отдельно. Новая append
  schema16 добавляет immutable capture anchors/current checkpoints и staged cleanup,
  schema17 — chat/message heads, retained generations и erasure fences.
  Schema18 добавляет отдельный checkpoint для staged результата локальной ASR.
  Schema19 фиксирует анонимные метки или пропуск и отдельный переход Ready.
  Schema20 сохраняет результат генерации саммари и отдельный receipt
  успешного завершения fixture job.
  Schema21 хранит отдельные разрешения MCP на чтение одной библиотеки,
  хеши секретов и отзыв. Отдельный endpoint для чтения проверяется на fixture.
  Windows pairing хранит секрет в Credential Manager и отдаёт клиенту
  открытый ID подключения. Отзыв проверяется на каждом чтении.
  Schema22 добавляет сохранённое подтверждение сырого видео и независимый
  terminal outcome Soniox.
- Bounded RepositoryClient/desktop bootstrap, metadata/content fragment reads,
  revisions/CAS, folder/tag organization, exact template staging, bounded chat/history
  reads и full native
  configuration без provider/consent changes. Shared native value models и portable
  share rendering извлечены в Tauri-free boundaries.
- Reserved/begun/abort/reattach/finalize capture с kernel owner proof и sealed
  recovery. Begin не принимает terminal seal. Unused abort сохраняет raw evidence.
  Failed finalize сохраняет частичную запись и подтверждённые реплики без
  постановки job. Sealed finalize и recovery проверяют журнал по кадрам под
  существующей блокировкой и не публикуют owner metadata. Обычный recorder
  ещё не подключён к этому protocol. Изолированный GUI использует нативный
  микрофонный writer, а service выполняет Prepare, Begin и Finalize.
- Registered post-process list/claim/renew/fail, lease-bound transcript/media reads,
  guard-bearing SDK owner, automatic title и whole-result automatic overlays.
  Cleanup меняет checkpoint/job/token атомарно, сохраняет raw/manual/provenance/voice
  caches и не продлевает lease. No-op тоже меняет checkpoint. Ready и job completion
  остаются отдельными переходами; native Ready предшествует optional stages.
- Постоянные media gates блокируют whole-meeting purge до final read clone.
  Windows OwnedMediaChild удерживает pin после parent death. Отдельный piped stdout
  не меняет прежний spawn, не обрезает JSON tail лимитом stderr и требует от caller
  concurrent drain/budget/deadline. Native engine routes не активированы; writer
  drain и неизвестные grandchildren этим не доказаны.

Проверки и evidence: [этап 0](../test-reports/2026-11-02-mcp-phase-0.md),
[Windows baseline](../test-reports/2026-11-02-mcp-windows-baseline.md),
[этап 1](../test-reports/2026-11-02-mcp-phase-1.md),
[этап 2](../test-reports/2026-11-02-mcp-phase-2.md),
[актуальный этап 3](../test-reports/2026-11-02-mcp-phase-3.md).
Полный lean snapshot schema21 со stdio прошёл 657 tests и один ignored helper.
Owned-process проверки покрывают разметку, Ready, очистку, публикацию саммари,
завершение job, восстановление failed capture и репетицию переключения. Lean и Windows
desktop/capture all-targets Clippy и MCP Inspector 2.13.0 прошли.
Native Windows schema21 прошёл 63 library tests и 2 проверки startup gate.
Явный тест синтетического импорта относится к предыдущему schema20 прогону.
Целевая выборка schema22 прошла 619 тестов и doc-tests; тест Windows Credential
Manager прошёл отдельным запуском. Проверки Inspector включают выдачу, чтение
и отзыв разрешения через отдельный IPC на синтетической библиотеке.
Прошли typecheck, 17 целевых тестов GUI, сборка frontend и Clippy обеих выборок.
Нативный Windows прогон schema22: 67 library tests и две проверки startup gate.
Обычный GUI сохраняет прежний Store routing. Физический capture/A-V и
packaged GUI текущего snapshot проверяются отдельно.
Целевые schema16 checks: checkpoint4, API6, client7, Store cleanup16, IPC cleanup2.
Целевые schema17 checks: API10, client8, Store12, IPC2 (giant fields/order/reconnect
и оба erase paths с pending global assistants/history retention).
Текущий capture snapshot прошёл 108 tests плюс 5 explicit synthetic FFmpeg и Clippy.
Packaged GUI evidence принадлежит v12 fixture, не schema16/production GUI.

Lean target: `.git/local-tests/20261102-mcp/target`. Освобождённый disposable native
`.git/local-tests/20261102-voice-reliability/target` согласован для MCP checks;
оба build/CMake jobs ограничены двумя. Normal target/release, frozen voice CLI,
acceptance/runtime.next1, runtime-production.next1 и corpus/evidence не изменять.
В voice chat больше не отправлять status/ACK; координация и отчёт только здесь.

Следующее действие: закрыть native provider/source/body checkpoints и bounded
контракты оставшихся GUI данных, затем переводить конкретные writers на
RepositoryClient с invalidation и regression на disposable library. Не делать
общий rebind RPC, не усыновлять current revisions при конфликте и не объявлять
Ready завершением всей job. Provider/consent требуют permission epoch,
coordinated Soniox cancel/ACK и fail-closed control outage. Handoff64MiB/200000 records и нынешний
DB finalize8MiB/10000 captions ещё требуют согласованного staged protocol;
prototype limits не становятся новыми recording defaults.
Покадровое чтение устраняет отдельную копию всего журнала в сервисе. Для
длинных handoffs ещё нужно ограничить память при записи raw/legacy JSON,
перестроении индексов и чтении больших источников, затем измерить время
транзакции и восстановления до расширения лимитов.

Для начатой записи без итогового манифеста сервис удерживает существующую
блокировку файлов записи от проверки идентификатора владельца до транзакции
SQLite. После аварийного завершения это освобождает библиотеку для следующей
записи. Встреча получает статус ошибки с длительностью доступной части WAV
и проверенным префиксом журнала субтитров в пределах лимита финализации.
Оригинальные файлы остаются на месте. Сервис запускает автоматическую
обработку только для штатно завершённой записи. Незавершённое видео остаётся
в исходном файле без публикации в плеере.

Source checkpoint локальной ASR реализован на synthetic library. Он фиксирует
фактическую модель и статус, сохраняет capture anchor и обновляет raw source,
provenance и индексы атомарно. Reclaim возвращает текущую фазу и исход ASR,
включая отличие пустого успешного результата от отсутствия модели.

Schema19 реализует первый speaker result для текущего local ASR source.
Анонимные кластеры и явный пропуск атомарно сохраняются вместе с проекциями
текущих меток. Source-owned overlay сохраняет архивную разметку и отличает
снятие метки от отсутствия overlay. Отдельный переход Ready оставляет job
активной для optional stages и сохраняет исходный срок lease. При reclaim
доступны speaker outcome и флаг cleanup после Ready.

Schema20 добавляет генерацию саммари и завершение fixture job после сохранённой
очистки или на проверенном пустом транскрипте. Результат саммари требует
совпадения ревизий транскрипта, саммари и ревизии и поколения шаблона.
Завершение использует сохранённый результат, отзывает claim и сохраняет
исторический receipt. Эмбеддинги и webhook отмечаются как вне fixture scope.
Для native cutover нужны сохранённые подтверждения этих эффектов или явный
пропуск. Публичный fixture adapter также читает саммари, папки, теги и заметки.
Заметку можно открыть по `note_id` из поискового результата.
Для нативной маршрутизации ещё нужны подтверждённые результаты
Soniox, разрешения провайдера и контракты голосовых подсказок. Soniox требует
явного статуса завершения распознавания отдельно от результата записи медиа.
Реальные имена и голосовые подсказки требуют существующих проверок качества и
защиты от позднего результата после смены голосового профиля.

Chat read transport реализован: отдельные metadata/messages/erasure revisions,
stable original ordering и bounded fragments. Staged writes с run/source fencing
пока отсутствуют. Transcript/meeting erase
очищает citations scoped/library-wide chats и должен инвалидировать completion
в той же транзакции, включая empty pending assistant; text/history сохраняются.
Новые write quotas остаются private prototype до native cutover.

Native chat scope сохраняется точно: meeting/None и все all/* читают всю
библиотеку; Some(empty), whitespace, Unicode и NUL не нормализуются. Roles и
citations не ограничены user/assistant или Citation[]: citations — arbitrary JSON
Value с fallback [] при невалидном JSON. Нужны fragments также для giant role и
scope metadata, raw SQLite timestamp sort keys и stable rowid tie. Erasure должен
инвалидировать library-wide pending [] assistants, включая meeting/None, даже
если citation SQL не нашёл ни одной ссылки. Token/done events не менять.

Сохранить voice quality seams: policy-versioned cache без fallback, naming/reuse
checks, checked preclear/final writes, без profile-centroid substitution, merge
очищает обе labels. Общий transaction-local helper legacy/service replace/delete
очищает точные meeting query caches всех policy namespaces; meeting delete также
очищает highlights. Global profiles/samples и clean-only edits сохраняются.

Перед production включением нужны stopped-owner snapshot/rollout, zero-Store
GUI/worker routing и native regression. Public projection/large-body fragments,
MCP handlers/grants и managed shares следуют своим этапам 4–6; native macOS,
hardware capture/A-V и реальные MCP hosts остаются отдельной приёмкой.

`bazza-service rehearse-adoption` проверяет миграцию закрытой legacy-базы на
двух отдельных копиях. `snapshot/rollback` содержит исходную схему, а
`snapshot/candidate` содержит текущую схему и сервисную identity. Команда
отклоняет работающего владельца, сохраняет исходную базу и публикует обе копии
после успешной проверки candidate. Медиа и активная библиотека остаются вне
репетиции переключения.

Терминальный формат v2 фиксирует `screen.raw.mp4`, таймлайн, доступные часы
и исход writer. Изолированный GUI умеет явно включить основной экран.
Сервис сверяет размеры под блокировкой и сохраняет доказательство в
транзакции Finalize. Ошибка необязательного видео сохраняет аудиозапись.
Готовое видео по-прежнему требует публикации `screen.mp4` последующим этапом.
Существующий формат v1 и повтор его квитанции используют прежний хеш.

В изолированном GUI удаление проходит через RepositoryClient и очередь
очистки сервиса. Обычный GUI ещё использует Store. Soniox может передать
подтверждённый журнал и отдельный исход распознавания; запуск и отзыв
облачного worker по сервисному протоколу остаются следующим шагом.
Для разрешений рабочей библиотеки нужен отдельный доверенный канал выдачи
прав. В fixture CLI список разрешений позволяет найти и отозвать подключение
после потерянного ответа команды выдачи.

Изолированный запуск загружает настройки, локаль и каталог шаблонов через
RepositoryClient до создания окна. Локальное состояние движков вынесено из
владельца Store. Обычный и сервисный рекордеры используют общую настройку
аудио и видео и общий переход Pause/Resume. Тестовый захват может использовать
сохранённые аудиоустройства и качество видео. Режим «звук приложения» требует
явно выбранного процесса перед подключением per-app loopback.

Нативный жизненный цикл Soniox проверяет привязку короткого разрешения к захвату
и эпохам, закрывает облачный PCM при отзыве или истечении и подтверждает
остановку после записи подтверждённых реплик. Для подключения к recorder
нужен сервисный источник разрешений и отзыва. Версия разрешения должна
меняться при отзыве, смене провайдера или ключа, даже если прежнее значение
потом восстановили. Отсчитывать короткий срок от монотонного времени отправки
запроса, чтобы задержка ответа не продлевала право на отправку PCM. Ключ
передавать только при начальной выдаче после проверки lease и владельца
артефактов. Обычный GUI ещё требуется перевести на RepositoryClient до
миграции библиотеки.

`--service-desktop` запускает обычные окна Library, Popup и RecordBar на явно
помеченной тестовой библиотеке. Штатные IPC списка, переименования, удаления,
настроек picker и управления записью выбирают сервисного владельца. Страница
встречи ждёт полного ответа сервиса и доступа к медиа с блокировкой удаления.
Обязательная постобработка пока ждёт переноса исполнения и публикации результата.

Schema23 хранит отдельную версию разрешения Soniox и закрытый журнал разрешений
по захвату. Сервис проверяет PID, proof, fence, Begin, владельца артефактов и
выбранный режим до передачи ключа. Обновление выдаёт только разрешение сроком
до восьми секунд. Изменение разрешающих настроек или ключа меняет версию;
подтверждение остановки принимается для прежнего разрешения после drain.

Для рабочей библиотеки принят сценарий с нуля. Перенос и удаление старой
базы в него не входят. `bazza-service init-library` резервирует новый
абсолютный каталог; сервис проверяет идентификатор и актуальную схему без
автоматической миграции. `bazza --service-library` запускает окна записи и
библиотеки на выбранном сервисе. `serve-library-external` отдельно включает
MCP endpoint, а `grant-library-read` выдаёт отзываемое разрешение на чтение.
Команды подключения описаны в [инструкции MCP](../../../crates/bazza-mcp/README.md).

Для изолированной сборки Windows со встроенным интерфейсом передавать
`build.frontendDist` относительно `src-tauri`. Абсолютный путь с буквой
диска Tauri может разобрать как URL и не встроить файлы.

После Stop нативный исполнитель берёт задание сервиса, использует закреплённый
WAV для Whisper либо подтверждённую расшифровку Soniox, затем публикует
диаризацию и доступное видео. Страница встречи читает версии расшифровки и
проигрывает закреплённые файлы через закрытый протокол WebView. Для включения
этого режима по умолчанию остаются проверки физической записи и воспроизведения,
реальных MCP hosts и перенос остальных путей приложения. Необязательные
embeddings и webhook пока остаются вне исполнителя сервиса.

На Windows для схемы 25 проверены запуск управляемой библиотеки, выдача
разрешения в Credential Manager и отказ MCP Inspector после отзыва. В
отдельной библиотеке собранное приложение распознало тестовый WAV через
Whisper Base и довело встречу до `Ready`. Проверка публикации видео
использовала короткий MP4, созданный комплектным FFmpeg. Микрофон, экран
и облачный Soniox при этих проверках не использовались.

В следующем прогоне Windows записаны физический USB-микрофон и отдельное
синтетическое окно. Готовый MP4 декодируется с аудио и видео и
воспроизводится в Bazza. MCP Inspector прочитал карточку этой встречи и
три сегмента расшифровки. Отдельный сеанс отправил только синтетическую
речь в реальный Soniox и сохранил три подтверждённых фрагмента.

Конкурентное восстановление могло занять блокировку между Stop и получением
ответа. Сервис теперь кратко проверяет появление подтверждения для той же
аренды. Добавлен Windows-тест этой гонки. Для локального запуска добавлен
пользовательский ярлык в меню «Пуск» и выбран каталог библиотеки вне кэша
пакета Codex. Идентификатор библиотеки и разрешение MCP сохранились.

На Windows Codex CLI 0.166.0-alpha.5.1 подключился к рабочей библиотеке
через локальный app-server и общую конфигурацию Codex. Для Codex выдано
отдельное разрешение на чтение. App-server обнаружил девять MCP-инструментов,
нашёл тестовую запись по фразе и прочитал три сегмента исходной расшифровки.
Повторное чтение в том же сеансе через 65 секунд вернуло те же сегменты.
Локальная задача Windows запускает сервис по требованию с правами текущего
пользователя. Холодный запуск с закрытым Bazza прошёл, и тот же процесс
сервиса продолжил работать после завершения проверочного app-server.

### Установка MCP на Windows

Область текущего этапа — локальная per-user установка через NSIS. В установку
входят сервис и MCP-адаптер. Подтверждение подключения выдаёт отдельное
разрешение Codex на чтение новой библиотеки. При отказе Bazza использует ту же
сервисную библиотеку. Существующая библиотека legacy остаётся доступна в
прежнем режиме. Сторонние записи в конфигурации Codex сохраняются.

Windows Task Scheduler запускает один экземпляр сервиса по требованию.
Адаптер заново подключается к тому же идентификатору библиотеки после простоя.
Переключатель в настройках отзывает разрешение без остановки сервиса записи.
Повторная установка сохраняет отключённое состояние.

Локальный Windows NSIS проверен на отдельном профиле с реальным Codex.
Установка создала библиотеку и подключение. Один сеанс Codex прочитал
статус до и после остановки сервиса. Отключение закрыло доступ уже открытому
клиенту. После повторной установки выбор сохранился. Включение выдало новое
разрешение, которое прочитал новый сеанс Codex. Удаление программы очистило
подключение и сохранило библиотеку и посторонние настройки Codex.

Установщик Windows также предлагает подключить Claude Code и Claude Desktop.
Для них созданы отдельные разрешения и отдельные переключатели. Настройки
обновляются в пользовательском профиле каждого клиента, а запуск сервиса
по требованию использует тот же проверенный механизм Windows. Автоматическое
обновление установщика сохраняет выбор и не выдаёт нового разрешения Claude.

Claude Code 2.1.316 на Windows дважды прочитал синтетическую расшифровку
через `bazza_get_transcript` в новых процессах Claude при закрытом Bazza.
Между чтениями сервис был остановлен и запустился по запросу. Сгенерированная
запись Claude Desktop прошла такой же тест чтения на уровне stdio-адаптера.
Отзыв разрешения одного клиента сохранял доступ второго клиента и Codex.
Отдельный прогон установленного NSIS-пакета подтвердил обнаружение
подключения в Claude Code, чтение через конфигурацию Desktop после
перезапуска сервиса, отзыв разрешений и удаление только настроек Bazza.
В авторизованном Claude Desktop 4.4212 после полного перезапуска и при
закрытом Bazza обычный чат вызвал временное подключение MCP и вернул
случайный код из исходной синтетической расшифровки. Код не передавался
в запросе. Проверка использовала отдельную библиотеку.
