# MCP для Bazza: исследование и проект интеграции

Дата: 2 ноября 2026 года. Статус: исследование и архитектурное предложение; MCP ещё не реализован. Требования уточнены пользователем после первоначального исследования — см. следующий раздел.

[Development index](../README.md) · [Архитектура](../architecture.md) · [Privacy](../../privacy.md)

Подробный порядок будущей реализации, зависимости и критерии приёмки: [план MCP](../plans/2026-11-02-mcp-implementation.md). План следует уточнённым требованиям пользователя ниже.

## Уточнённые требования: вся библиотека и работа без GUI

Пользователь уточнил целевой сценарий:

- Читать саммари, собственные заметки, транскрипты, участников, календарные события и доступные общие встречи.
- Создавать, редактировать и удалять данные, управлять sharing.
- Искать по названиям встреч, транскриптам, саммари и собственным заметкам.
- Выдавать разрешение на библиотеку целиком, без согласования каждой встречи.
- Работать при полностью закрытом desktop GUI: MCP обращается к локальным данным самостоятельно.

**Все пункты реализуемы через MCP.** Ограничения первоначального варианта ниже — обязательный открытый GUI, per-meeting grants и только UI proposals для sharing — не являются требованиями протокола и больше не соответствуют выбранному сценарию пользователя. Вместе с тем возможность программы выполнять операции и собственные approval-настройки AI-клиента — разные уровни: host может дополнительно спрашивать подтверждение вызова. [MCP Tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools).

### Новая архитектурная цель

~~~text
AI-клиент → bazza-mcp → автономное ядро данных/сервис Bazza → bazza.db + файлы
Desktop GUI → то же ядро данных/сервис
~~~

MCP — самостоятельный локальный executable. Клиент может запускать его по stdio без открытия интерфейса Bazza. Это обычная модель локального MCP server. [stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio).

Нужное общее ядро должно быть Tauri-free и содержать предметные операции хранения, индексации, revisions, удаления связанных файлов и управления sharing. Нельзя просто перенести Tauri AppState в скрытый процесс: запуск доступа к базе не должен включать capture, detection, UI recovery или тяжёлые модели автоматически. Сам агент получает tools; SQL и приватные настройки остаются внутренней реализацией.

Возможны два способа размещения этого ядра:

| Вариант | Когда подходит | Что требуется |
| --- | --- | --- |
| Ядро внутри самостоятельного bazza-mcp; GUI использует ту же библиотеку | Начальные CRUD/search, без обязательного постоянно работающего процесса | Совместимые migrations, SQLite concurrency, cross-process guards, revisions всех writers, согласование файловых операций и обновление UI |
| Отдельный headless backend; GUI и MCP — его клиенты | Единое владение jobs/файлами, несколько агентов, календарное обновление и активный LAN sharing | Lifecycle и локальный authenticated IPC; GUI может завершаться независимо |

Для полного набора функций предпочтителен отдельный headless backend, который можно запускать по требованию. Он самостоятельно открывает библиотеку и не зависит от жизни окна. GUI перестаёт быть единственным владельцем mutable runtime. Обязательный системный autostart из этого не следует.

Для чтения и CRUD достаточно работающего MCP/backend, запущенного клиентом. Если share-ссылки должны обслуживаться даже после отключения MCP-клиента, backend должен продолжать работать, пока включён sharing. Если завершены все процессы, SQLite сохраняет данные и настройки доступа, но сама не обслуживает сетевые ссылки.

### Что уже есть и что нужно добавить

| Требование | Текущее состояние Bazza | Необходимая работа |
| --- | --- | --- |
| Саммари, транскрипты, участники | Сохраняются локально | Предметные read/write tools, revisions, безопасные DTO |
| Собственные заметки | Отдельной persistent notes-сущности и CRUD не обнаружено; live notes — AI-текст в памяти | Notes table/model, связь со встречей, CRUD, revisions и индекс |
| Календарные события при закрытом GUI | События находятся в AppState.calendar; сохраняются settings и calendar_uid встречи | Persistent calendar cache, source identity, fetched_at/refresh status и headless refresh |
| Создание/изменение календаря у провайдера | Существующая интеграция читает ICS | Отдельный provider API/connector и его права; локальная запись в БД внешний календарь не изменяет |
| Доступные общие встречи | Есть исходящие shares локальных встреч; отдельного inbox/catalog чужих встреч нет | Для собственных shares — существующие сущности; для полученных — source/access model, cache или явный import |
| Поиск по четырём источникам | Есть title/segment search и chunk retrieval; саммари и own notes не включены в единый полнотекстовый поиск | Unified FTS index: title, effective transcript, summary, user note; source_kind и citations |
| CRUD при закрытом GUI | Значительная orchestration находится в Tauri commands | Извлечение общей Tauri-free логики, cross-process/runtime coordination и файловая consistency |
| Sharing через MCP | Store хранит shares; LAN server управляется desktop | Headless lifecycle сервера; create/update/revoke tools и общий grant на эти операции |

Основания: [миграции](../../../crates/bazza-store/src/migrations.rs), [live notes](../../../src-tauri/src/live_feed.rs), [calendar cache](../../../src-tauri/src/calendar.rs), [shares](../../../crates/bazza-store/src/organize.rs), [LAN server](../../../crates/bazza-share/src/server.rs).

Разрешение на всю библиотеку включает текущие и будущие встречи этой библиотеки. При подключении можно один раз выбрать операции чтения, записи, удаления и sharing; разрешённые операции не требуют per-meeting разрешения. Это не разрешение выдавать агенту credentials из settings и не расширение исходных прав на удалённые чужие встречи. Доступ к ним ограничен правами, реально выданными источником.

Создание карточки встречи, собственных заметок или импорт готового транскрипта возможно без начала записи. Поддержку пустой/text-only встречи и отсутствующего media нужно явно добавить в модель и UI. Редактирование транскрипта сохраняет исходный raw text и изменяет cleaned overlay; удаление встречи удаляет её данные и связанные файлы согласованно.

### Обновлённая последовательность

1. Общий Tauri-free data service и headless host; единый grant на всю библиотеку; согласованный schema/lifecycle.
2. Чтение и unified search с citations; persistent user notes и calendar cache.
3. CRUD, revisions, idempotency, согласованное удаление файлов и обновление открытого GUI.
4. Headless sharing и при необходимости полученные shared meetings с учётом source permissions.
5. Проверки при закрытом GUI, одновременной работе GUI/нескольких агентов, restart и отзыве общего grant.

Это уточнение меняет проект интеграции, но ещё не меняет поведение работающего Bazza или существующую privacy-документацию: код не реализован. Нативная запись, автоматическое подтверждение voice identity и публикация этого fork не входят в уточнённый запрос.

## Первоначальная рекомендация и подробный технический анализ

Далее сохранён исходный ограниченный вариант с открытым приложением для сравнения. Выбор архитектуры, гранулярность grants, APP_NOT_RUNNING как штатная зависимость и UI-only sharing в разделах 5, 7, 10, 12, 14 и 15 заменены уточнёнными требованиями выше. Анализ secrets, domain invariants, raw/clean, протокола, citations, jobs и проверок остаётся применимым.

Bazza стоит сделать MCP-сервером для внешних AI-клиентов. Основной вход — небольшой локальный Rust executable bazza-mcp, запущенный клиентом по stdio. Он обращается через приватный канал к работающему desktop-приложению. Общие application services проверяют права и вызывают существующие Store, retrieval и фоновые операции.

Это позволит агенту находить решения в архиве, получать проверяемые цитаты, читать выбранные встречи, предлагать исправления и управлять разрешёнными функциями приложения. Пользователю не придётся объяснять схему SQLite или копировать многочасовые транскрипты в чат.

Начинать следует с чтения и поиска. После этого — разрешённые изменения названий, тегов, папок и cleaned-транскрипта с проверкой версии. Запись, подтверждение личности говорящего и создание share остаются действиями пользователя в Bazza. Удалённый доступ, headless-владелец библиотеки и публичное распространение не входят в начальный проект.

Первый полезный результат: запрос «Что решили по срокам проекта за последние две недели?» → поиск в разрешённых встречах → несколько точных реплик с датами и таймкодами → ответ внешнего агента → возможность открыть источник в Bazza.

## 1. Основания и границы исследования

Исследованы текущие исходники Bazza: хранилище, миграции, retrieval, AppState, Tauri commands, typed UI IPC, pipeline, summary-runner и документация границ данных. Три независимых направления проверки: хранение, приложение и актуальный стандарт MCP. Рабочее дерево уже содержит многочисленные локальные изменения; выводы относятся к прочитанному состоянию файлов, а не к чистому upstream или ранее проверенному build.

Внешние сведения проверены по официальной спецификации MCP, официальным SDK и developer tools, документации OpenAI и Claude Code, инженерным материалам Anthropic и первичному описанию meeting-интеграции Granola. Требования стандарта отмечены MUST/SHOULD; остальные решения — рекомендации для Bazza.

Пользовательская база, записи, транскрипты и credentials не открывались. Приложение, захват, inference и внешние агенты на пользовательских встречах не запускались. Прототип MCP не собирался. Результаты исследований не означают runtime-приёмку предложенной архитектуры.

## 2. Актуальный стандарт: существенное изменение в 2026 году

На дату исследования стабильная редакция — **2026-07-28**. Старые руководства с 2025-11-25 как latest уже не подходят для нового проекта без проверки совместимости. [MCP specification](https://modelcontextprotocol.io/specification/2026-07-28), [Versioning](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning).

| Область | 2025-11-25 и раньше | 2026-07-28 | Решение для Bazza |
| --- | --- | --- | --- |
| Начало работы | initialize handshake | Без handshake; server/discover для discovery, версия и capabilities в каждом запросе | SDK с поддержкой обеих моделей |
| Состояние протокола | Есть connection/session semantics | Stateless; межвызовное состояние передаётся handles | job_id, request_id и revision принадлежат Bazza |
| HTTP | Возможны session ID и GET stream | POST; progress в response stream запроса, change notifications через subscriptions/listen | Не копировать прежние HTTP-примеры |
| Дополнительный ввод | Server-initiated requests | Multi Round-Trip Requests, input_required | UI-подтверждения не должны зависеть только от поддержки клиента |
| Результаты | Прежние envelopes | resultType у результатов | Формирует SDK |
| Долгие операции | Experimental tasks | Отдельное расширение Tasks | Обычные domain jobs как переносимая основа |

Эти изменения описаны в [официальном changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog). Протокольную совместимость нужно проверять отдельно от бизнес-операций.

Roots, Sampling, MCP Logging и Dynamic Client Registration в новой редакции deprecated: новым реализациям SHOULD NOT строиться на них. Старый HTTP+SSE также deprecated. Это не запрет SSE внутри Streamable HTTP. [Deprecated Features](https://modelcontextprotocol.io/specification/2026-07-28/deprecated).

Практическое следствие: Bazza не нужен собственный MCP-клиент или Sampling, чтобы внешний агент работал с библиотекой. Нужен сервер. Существующие интеграции Bazza с LLM сохраняют отдельную роль.

## 3. Лучшие практики проектирования MCP

### Предметные операции, понятные агенту

Инструмент должен решать узнаваемую задачу: найти фрагменты, прочитать встречу, исправить cleaned-текст. Стоит избегать десятков почти одинаковых инструментов и универсального action с непредсказуемыми аргументами. Описания должны объяснять условия использования, область данных, побочные эффекты и ограничения. Качество проверяется задачами агента, числом вызовов и точностью результата, а не количеством опубликованных endpoints. Это соответствует рекомендациям [Anthropic о проектировании tools](https://www.anthropic.com/research/writing-tools-for-agents).

Для Bazza предлагаются следующие правила:

1. Различать обзор библиотеки и поиск содержания. list_meetings предназначен для хронологии; search — для обнаружения релевантных реплик.
2. При чтении встречи возвращать компактные метаданные. Транскрипт и итог получать отдельно.
3. Отдавать ограниченные страницы и диапазоны времени. Никогда не загружать всю библиотеку как ответ на общий вопрос.
4. Сохранять машиночитаемые IDs, даты и времена. Человекочитаемое название дополняет идентификатор.
5. В каждом результате поиска показывать источник и фактически применённый способ поиска.
6. Проверять разрешения до выбора и ранжирования данных; скрытые встречи не должны попадать в snippets, counts или сообщения об ошибке.
7. Отделять чтение, изменение данных, запуск вычислений и запрос действия в UI.
8. Называть результат частичного успеха прямо: «правка сохранена, semantic index ожидает обновления».
9. Не выполнять LLM-запросы как побочный эффект поиска. Ответ строит выбранный пользователем внешний агент.
10. Сохранять стабильные имена и порядок tools; изменения контрактов версионировать.

### Схемы и результаты

Tools используют JSON Schema; в актуальной спецификации базовый dialect — 2020-12. Если объявлен outputSchema, structuredContent MUST соответствовать ему. Для совместимости SHOULD также возвращать JSON в TextContent. Annotations описывают свойства операции, но не выполняют авторизацию. [MCP Tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools).

Для начального контракта Bazza разумно использовать object на верхнем уровне, простые enum, явные единицы времени, ограниченные строки и массивы. Ошибки выполнения инструмента обозначать isError и стабильным кодом домена; malformed JSON-RPC, неизвестный метод и ошибки протокола оставлять протокольному слою SDK. Инструмент должен валидировать входы независимо от валидации клиента.

Предлагаемые значения по умолчанию, которые ещё нужно измерить: 20 результатов поиска, максимум 100; до 100 сегментов или 64 KiB текста на страницу. Если один сегмент превышает бюджет страницы, контракт должен поддерживать продолжение его текста. Нельзя молча отрезать сегмент и объявлять транскрипт полным.

### Проверяемость ответа

Для каждого найденного фрагмента нужны meeting_id, transcript_revision, segment_id или диапазон сегментов, start_ms/end_ms, representation, краткий excerpt и logical source URI. После полной re-transcription segment IDs могут измениться; ссылка должна быть привязана к версии транскрипта, а устаревшая версия — явно определяться.

Chunk row ID не подходит как постоянный публичный ID: chunks пересоздаются. Для перехода в приложение полезен отдельный open_meeting с таймкодом. MCP URI сам по себе не означает, что операционная система уже умеет открыть такой адрес.

## 4. Что в Bazza уже готово

| Подтверждённое свойство | Польза и ограничение для MCP | Источник |
| --- | --- | --- |
| Tauri-free Store, SQLite/FTS5, единый mutex connection | Можно переиспользовать доменные запросы; mutex действует внутри процесса | [db.rs](../../../crates/bazza-store/src/db.rs), строки 52–76 |
| Store::open включает WAL и выполняет миграции | Не использовать его как «безопасный read-only opener» | [db.rs](../../../crates/bazza-store/src/db.rs), строки 60–76 |
| Сегменты имеют raw text, clean_text и manual flag | Можно честно выдавать raw/effective/both | [segments.rs](../../../crates/bazza-store/src/segments.rs), строки 6–16 |
| Правка overlay проверяет ready и принадлежность сегмента, обновляет chunks транзакционно | Основа безопасного correction tool | [segments.rs](../../../crates/bazza-store/src/segments.rs), строки 124–144 |
| Segment FTS, chunk BM25, vectors и RRF уже реализованы | MCP не требует новой поисковой инфраструктуры для первого этапа | [ai.rs](../../../crates/bazza-store/src/ai.rs), строки 211–300; [vectors.rs](../../../crates/bazza-store/src/vectors.rs), строки 24–125 |
| Hybrid retrieval находится в Tauri orchestration | Потребуется извлечь общий сервис и явную диагностику ошибок | [embed_stage.rs](../../../src-tauri/src/embed_stage.rs), строки 23–36 |
| AppState владеет Store, capture, engines, cancel maps и share server | Работающее приложение — естественный владелец mutable runtime | [state.rs](../../../src-tauri/src/state.rs), строки 26–62 |
| Есть typed IPC и предметные commands | Можно постепенно извлекать services из существующих операций | [ipc.ts](../../../app/src/lib/ipc.ts), [commands](../../../src-tauri/src/commands/mod.rs) |
| Summary jobs уже имеют отмену; processing передаёт события | Есть исходная orchestration, но нет общего durable job API | [ai.rs](../../../src-tauri/src/commands/ai.rs), строки 52–101; [events.rs](../../../src-tauri/src/events.rs) |
| Данные размещаются в Bazza data-dir с возможным pointer | Bridge должен работать с активной библиотекой, без db_path от агента | [db.rs](../../../crates/bazza-store/src/db.rs), строки 6–37 |

В исследованном дереве не обнаружен публичный MCP server или универсальный внешний automation bridge. LAN share server и BAZZA_SMOKE имеют другие назначения. Smoke mode захватывает реальное аудио и не подходит как автоматический способ тестирования MCP.

### Почему нельзя просто открыть SQLite

Read-only SQL тоже может раскрывать лишнее: settings содержит private Soniox key, LLM key, секретные календарные URL и настройки webhook. Запрет UPDATE не решает эту проблему. [Store settings](../../../crates/bazza-store/src/settings.rs), [STT credential storage](../../../src-tauri/src/stt_stage.rs), [Settings](../../../src-tauri/src/settings.rs).

Изменения связаны с производными данными и runtime: исправление текста влияет на FTS/chunks/vectors; speaker rename может объединить labels, обучить voice memory и вызвать обновление итогов; удаление затрагивает файлы и jobs. Прямой SQL обойдёт эти операции. [Transcript correction](../../../src-tauri/src/commands/meetings.rs), [Speaker operations](../../../src-tauri/src/commands/speakers.rs).

WAL поддерживает несколько SQLite connections. Риск здесь — обход application invariants и несовместимые миграции, а не доказанная «поломка SQLite от второго подключения». Для отдельного аналитического режима позже допустимы allowlisted projections на проверенном read-only snapshot.

### Почему нельзя автоматически экспортировать весь Tauri IPC

get_settings возвращает полный Settings; get_meeting — абсолютные media paths; list_shares — bearer tokens и LAN URLs; save_text_file принимает путь записи; change_data_dir переносит библиотеку. Эти интерфейсы создавались для доверенного UI, их нельзя считать готовым контрактом внешнего агента. [Settings command](../../../src-tauri/src/commands/settings.rs), [Meeting commands](../../../src-tauri/src/commands/meetings.rs), [Organization commands](../../../src-tauri/src/commands/organize.rs).

Нужны отдельные MCP DTO с allowlist полей. Поля будущего settings не должны становиться доступны агенту автоматически.

## 5. Сравнение архитектур

| Вариант | Преимущества | Цена и ограничения | Оценка |
| --- | --- | --- | --- |
| Готовый generic SQLite MCP | Быстрый эксперимент с таблицами | Secrets, schema coupling, отсутствие runtime invariants и удобных citations | Не подходит как продуктовый API |
| Отдельный MCP, самостоятельно открывающий Store на запись | Rust reuse, работает без GUI | Второй владелец migrations/files/jobs, расходящиеся состояния приложения | Не выбирать для первого этапа |
| MCP Streamable HTTP внутри Bazza | Прямой вызов services, несколько клиентов | HTTP auth, Origin/Host, listener lifecycle, discoverable address | Дополнительный транспорт при реальной потребности |
| stdio MCP adapter → private bridge → Bazza | Широкая совместимость локальных клиентов, нет публичного listener; один runtime owner | Нужно реализовать небольшой внутренний RPC и pairing | Рекомендуемый основной вариант |
| Отдельный shared daemon, UI как клиент | Headless, единое владение всей системой | Большая переделка запуска, recovery, capture и packaging | Возможный поздний проект |

Рекомендуемая схема:

~~~mermaid
flowchart LR
    A["Внешний агент: Codex / Claude Code"] -->|MCP stdio| B["bazza-mcp"]
    B -->|Приватный IPC и credential| C["Bazza desktop: bridge"]
    U["React UI"] -->|Tauri commands| S["Application services + policy"]
    C --> S
    S --> D["Store: SQLite + files"]
    S --> J["Job registry + engines"]
    S --> Q["UI requests и подтверждения"]
    S --> E["События invalidation для окон"]
~~~

Внешние MCP-клиенты могут запускать по одному лёгкому adapter-процессу. Все они обращаются к одному работающему Bazza; adapter не загружает whisper/llama/sherpa, не запускает recovery и не открывает mutable Store.

### Предлагаемое размещение кода

Названия ниже — проектные; этих модулей сейчас нет.

| Место | Ответственность |
| --- | --- |
| crates/bazza-agent-api | Tauri-free DTO, schemas, machine error codes, policy types и bounded request contract |
| crates/bazza-mcp | rmcp server, tools/resources/prompts, stdio lifecycle; IPC client к Bazza |
| src-tauri/src/services | Общие application operations поверх AppState; сохранение guards и orchestration |
| src-tauri/src/agent_bridge.rs | Приём внутренних запросов, аутентификация, отправка в services |
| src-tauri/src/commands | Тонкие Tauri wrappers поверх тех же services |
| app/src/lib/ipc.ts и Settings | Управление grants, обзор подключений, отзыв и UI requests |

Чистые алгоритмы и policy остаются в crates; app orchestration — в src-tauri. Сначала достаточно извлечь несколько нужных операций. Предварительно переносить весь desktop в новый большой core не требуется. Если позже появится headless-host, service boundary можно расширить через Tauri-free runtime traits.

### Приватный канал

Для Windows кандидат — named pipe с ACL текущей пользовательской сессии и запретом remote clients; для macOS — Unix domain socket в приватном каталоге. Это внутренний application RPC, а не ещё один обязательный публичный транспорт MCP.

Граница канала должна иметь собственную аутентификацию, проверку peer/endpoint, максимальный размер сообщения, deadline и ограниченную очередь. App должен авторизовывать каждую операцию: проверкам adapter доверять недостаточно. Данные discovery endpoint не являются credential.

При закрытом Bazza adapter может обслужить protocol discovery и объяснить APP_NOT_RUNNING при обращении к библиотеке. Он не должен автоматически создавать вторую библиотеку, запускать hidden recorder или владеть приложением как дочерним процессом агента. Запуск UI по отдельному запросу возможен позже с явным UX.

## 6. SDK и transport

Предпочтителен официальный Rust SDK RMCP: он соответствует Rust backend Bazza и позволяет распространять локальный бинарник без обязательного Node/Python runtime. TypeScript и Python также имеют официальные SDK; их главное преимущество здесь — скорость отдельного прототипа, а цена — ещё одна runtime/package boundary. Rust сейчас указан как Tier 1 в [официальном каталоге SDK](https://modelcontextprotocol.io/docs/2026-07-28/sdk).

На дату исследования последний релиз Rust SDK — rmcp-v3.8.0 от 26 октября 2026 года. README этого тега подтверждает поддержку stable 2026-07-28 и предыдущих версий; Cargo.toml требует Rust 1.88. Лицензия SDK — Apache-2.0; лицензию Bazza и upstream attribution нужно сохранить. [Релизы](https://github.com/modelcontextprotocol/rust-sdk/releases), [README фиксированного тега](https://github.com/modelcontextprotocol/rust-sdk/blob/rmcp-v3.8.0/README.md), [Cargo.toml](https://github.com/modelcontextprotocol/rust-sdk/blob/rmcp-v3.8.0/Cargo.toml).

Следует закрепить проверенную версию и lockfile, обновлять после проверок совместимости и advisory, не брать main как production dependency. В Bazza rust-toolchain.toml сейчас задаёт stable, но доступность подходящего компилятора в этой shell не подтверждена: rustc отсутствует в PATH. Проверка SDK-сборки остаётся отдельной работой.

Для stdio MUST оставлять stdout только для протокольных сообщений. Логи — stderr; EOF stdin должен быстро завершать adapter. Он не должен печатать баннеры, транскрипты, startup diagnostics или сообщения installer в stdout. [stdio specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio).

Streamable HTTP добавлять при подтверждённой необходимости. Стандарт требует валидировать Origin; недопустимый присутствующий Origin → 403. Локальный listener SHOULD быть loopback и иметь authentication. Для Bazza дополнительно проверять Host, отклонять browser origins по умолчанию, ограничивать body/rate и не принимать redirects в неизвестные endpoints. Отсутствие Origin у native-клиента не является аутентификацией. [HTTP specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http).

Не использовать LAN share server для MCP: у него иной scope, получатели и жизненный цикл доступа.

## 7. Предлагаемый публичный контракт

Все имена и параметры в этом разделе — предложение. Количество инструментов следует уточнить по agent eval; семь read tools достаточно как исходный набор.

### Первый набор: чтение

| Tool | Назначение | Существенные параметры / результат |
| --- | --- | --- |
| bazza_get_app_status | Проверить доступность и состояние | app/API version, разрешённая library identity, состояние текущего подключения и безопасные capability flags |
| bazza_list_meetings | Просмотреть встречи по времени и фильтрам | from/to с timezone, status, folder/tag, cursor, limit; краткие metadata |
| bazza_search | Найти встречи и фрагменты содержания | query, allowed scope, date filters, representation, lexical/hybrid; bounded hits и diagnostics |
| bazza_get_meeting | Получить одну карточку | meeting_id; title, times, status, speakers, summary availability, revisions |
| bazza_get_transcript | Прочитать выбранный диапазон | meeting_id, representation, start/end_ms или cursor, expected_revision |
| bazza_get_summary | Прочитать сохранённый итог | meeting_id; markdown, provenance, revision, статус отсутствия |
| bazza_get_library_structure | Получить папки и теги | Только структура и названия в разрешённой области |

get_job добавляется вместе с общим job registry и инструментами запуска/отмены. Не изображать существующие summary cancel maps универсальным durable registry.

App status не раскрывает active meeting ID/title, глобальные job counts или чужие grants вне разрешённой области. Состояние recording/live выдаётся только в объёме соответствующего permission; иначе capability доступности не подтверждает наличие конкретной скрытой встречи.

Поиск нужен как самостоятельный tool, даже если есть resources. Рабочий сценарий должен проходить на клиенте с поддержкой только tools. Обычное чтение не скачивает модели и не вызывает LLM/cloud provider.

### Дополнение: контролируемые изменения и навигация

| Tool | Семантика | Условие |
| --- | --- | --- |
| bazza_update_meeting | Разрешённые metadata: title, folder, tags | expected_metadata_revision, idempotency_key, grant |
| bazza_set_transcript_corrections | Пакет cleaned overlays или reset | expected_transcript_revision, атомарный batch, raw сохраняется |
| bazza_save_analysis | Сохранить подготовленный внешним агентом результат | Новый versioned artifact с provenance и source revision |
| bazza_open_meeting | Открыть UI на встрече/таймкоде | Отдельное navigation permission |

metadata update лучше делать typed patch с разрешёнными полями и явной семантикой tag operations. Не принимать произвольный JSON Merge Patch в Store. Save analysis не должен молча перезаписывать текущий Summary: versioned analysis — предлагаемая новая сущность; если выбрать замену Summary, нужен отдельный review/replace контракт.

### Последующее расширение: вычисления и запросы действий приложения

| Tool / capability | Поведение |
| --- | --- |
| bazza_start_summary_job | Явно выбирает backend/template и сообщает recipient; возвращает job_id |
| bazza_get_job | Возвращает owner-scoped status, stage, outcome и result reference по job_id |
| bazza_cancel_job | Отменяет разрешённый job; сообщает фактический итог отмены |
| bazza_request_recording | Открывает штатный picker; запись начинается действием пользователя |
| bazza_propose_speaker_identity | Показывает изменение, возможный merge и последствия voice memory в UI |
| bazza_request_share | Показывает выбранную встречу и scope публикации; человек создаёт share |
| Import/export/retranscribe/reindex | Отдельные контракты, grants, paths и jobs после базовой приёмки |

Сейчас open_record_picker уже существует; open_main лишь фокусирует окно. Открытие конкретной meeting route с таймкодом ещё потребуется реализовать. Прямой агентский start/stop/pause recording — отдельное продуктовое решение, его нельзя вывести из разрешения читать архив. [Recording commands](../../../src-tauri/src/commands/recording.rs).

В начальный API не включать execute_sql, invoke_tauri_command, run_shell, read_file/write_file, set_setting, generic HTTP fetch, credentials, voiceprints, перенос библиотеки и произвольное удаление. Полезные пользовательские сценарии обслуживаются предметными инструментами.

## 8. Примеры контракта данных

### Поиск

Ниже — только domain payload, без JSON-RPC envelope SDK. IDs, текст и revision вымышленные.

~~~json
{
  "query": "кто согласовал перенос релиза",
  "scope": { "meeting_ids": ["11111111-1111-4111-8111-111111111111"] },
  "representation": "effective",
  "mode": "hybrid",
  "limit": 10
}
~~~

~~~json
{
  "schema_version": 1,
  "retrieval": {
    "requested": "hybrid",
    "used": "lexical",
    "degraded": true,
    "reason": "EMBEDDING_MODEL_UNAVAILABLE",
    "index_state": "partial"
  },
  "hits": [{
    "meeting_id": "11111111-1111-4111-8111-111111111111",
    "meeting_title": "Планирование релиза",
    "transcript_revision": "t:42",
    "segment_id": 173,
    "start_ms": 318000,
    "end_ms": 326400,
    "representation": "effective",
    "excerpt": "Перенос на пятницу согласован...",
    "source_uri": "bazza://meetings/11111111-1111-4111-8111-111111111111/transcript/t%3A42/segments/173"
  }],
  "next_cursor": null,
  "truncated": false
}
~~~

Отсутствие embedding model — допустимая деградация. Ошибка SQLite — SEARCH_UNAVAILABLE, а не пустой успешный массив. Нет совпадений — успешный пустой результат с рабочим индексом. Если доступна только часть индекса, ответ должен сообщать ограничение.

По умолчанию поиск использует effective text. Запрос, excerpt и citation должны относиться к одной representation и revision. Raw/both — осознанный параметр для сверки; при both каждый hit указывает своё представление. Нынешний Store::search ищет raw и clean, но формирует snippet из raw: его нельзя выдать за effective search без адаптации. Chunk retrieval использует effective text, однако chunk hit ещё нужно сопоставить с точными сегментами выбранной revision. Иначе агент может процитировать исправленную или намеренно скрытую реплику как актуальную. [Segment search](../../../crates/bazza-store/src/segments.rs), строки 147–170.

Клиенту не нужен абсолютный путь к mix.wav. source_uri — logical identifier, который сервер разрешает после проверки grant. Для media позже нужен отдельный access scope.

### Правка транскрипта

~~~json
{
  "meeting_id": "11111111-1111-4111-8111-111111111111",
  "expected_transcript_revision": "t:42",
  "idempotency_key": "22222222-2222-4222-8222-222222222222",
  "corrections": [
    { "segment_id": 173, "clean_text": "Перенос на пятницу согласован." },
    { "segment_id": 174, "clean_text": "" },
    { "segment_id": 175, "clean_text": null }
  ]
}
~~~

Семантика соответствует нынешней модели Bazza: строка — manual cleaned overlay; пустая строка — намеренно пустой overlay; null — reset к автоматическому/исходному представлению. Effective text вычисляется через null-aware выбор, а не проверку непустоты. Raw text, время и speaker labels этим инструментом не меняются.

Сейчас Store поддерживает одну правку; batch, revision precondition и idempotency надо добавить. Проверки всего batch выполняются в одной транзакции; один неверный/чужой сегмент отменяет пакет. Chunks rebuild выполняется один раз; вектора пересчитываются после commit с защитой от устаревшего текста.

Предлагаемый результат содержит new_revision, applied_count, index_state и warnings. Если lexical index обновлён, а embedding не готов, это сохранённая правка с предупреждением, а не повод повторять mutation.

### Ошибки

| Code | Что должен понять агент |
| --- | --- |
| APP_NOT_RUNNING | Открыть Bazza; повтор не должен создавать библиотеку |
| INTEGRATION_DISABLED / ACCESS_REVOKED | Требуется действие пользователя в настройках |
| NOT_FOUND_OR_NOT_AUTHORIZED | Не раскрывать наличие скрытой встречи |
| INVALID_ARGUMENT | Исправить конкретный параметр по схеме |
| REVISION_CONFLICT | Перечитать источник и заново подготовить изменение |
| MEETING_BUSY / ENGINE_BUSY | Повторить после разрешённого ожидания |
| SEARCH_UNAVAILABLE | Не делать вывод «в архиве нет информации» |
| APPROVAL_REQUIRED / USER_DECLINED | Действие ждёт человека / отклонено |
| JOB_INTERRUPTED | Проверить сохранённый результат и recovery status |

Raw SQLite errors, пути пользователя, токены и содержимое чужих встреч не должны попадать в публичную ошибку. Диагностика для разработчика хранится отдельно и ограниченно.

## 9. Resources и prompts

Resources полезны как адресуемые документы: metadata встречи, итог и ограниченный фрагмент транскрипта по URI template. Хост решает, как включать их в контекст. И resources/read, и search/get tools должны применять одну policy; URI не является разрешением доступа. [MCP Resources](https://modelcontextprotocol.io/specification/2026-07-28/server/resources).

Рекомендуемые логические адреса:

~~~text
bazza://meetings/{meeting_id}
bazza://meetings/{meeting_id}/summary
bazza://meetings/{meeting_id}/transcript/{revision}/segments/{segment_id}
~~~

Не делать resources/list выгрузкой всей библиотеки. Достаточно небольшого discovery и templates; содержимое обнаруживается поиском и доступно предметным get tool. Полный транскрипт всегда должен иметь ограниченный путь чтения.

В modern MCP cacheable results требуют ttlMs и cacheScope. Для личных данных и scope-dependent списков Bazza — private; на старте разумен ttlMs=0 для содержимого. Кэш не заменяет проверку прав; уже переданную клиенту копию данных отзыв grant не удаляет. [Caching](https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/caching).

Prompts можно добавить как необязательное удобство: подготовка к встрече, сравнение решений, извлечение задач с цитатами. Они не являются permission grants и не запускают публикацию. Их отсутствие в клиенте не должно мешать основным сценариям. [MCP Prompts](https://modelcontextprotocol.io/specification/2026-07-28/server/prompts).

Короткие server instructions могут объяснять порядок «search → read relevant ranges → cite» и требование проверять revision перед изменением. Содержимое встреч не должно попадать в инструкции, описания tools или advertising metadata.

## 10. Permissions и граница данных

### Предлагаемые profiles

| Profile | Возможности | Выбор пользователя |
| --- | --- | --- |
| Просмотр метаданных | Список/карточки встреч, папки/теги, app status | Разрешённые встречи или вся библиотека |
| Чтение содержания | Поиск реплик, transcript, summary | Отдельное согласие на передачу текста agent host |
| Организация | Названия, папки, теги | Можно выдать постоянное разрешение на выбранную область |
| Редактирование производного текста | Cleaned corrections, сохранение analysis | Отдельный scope; revision и provenance обязательны |
| Навигация | Открыть встречу или picker | Без скрытого начала capture |
| Вычисления/передача | Запуск backend, import/export/share requests | Явный recipient/destination и предметные ограничения |

Read-only — начальный режим, а не обещание, что текст остаётся на компьютере. После ответа MCP внешний host может отправить данные своему облачному провайдеру. Bazza должен объяснить это при подключении: локальны база и transport; обработка зависит от клиента. MCP не позволяет Bazza контролировать дальнейшее использование уже выданного текста.

Scopes нужны на стороне приложения для каждого вызова. Read grant не подразумевает live transcript, audio/video, глобальную voice memory или содержимое будущих папок автоматически. Если выбран grant на папку, UX должен прямо объяснять, включаются ли добавленные в неё впоследствии встречи. Простой начальный вариант — явный набор встреч; вся библиотека — отдельный выбор.

Пользователь должен видеть подключение, область данных, последние действия и кнопку отзыва. Самопровозглашённое clientInfo.name не является достоверной личностью; полномочия связываются с выданным credential. Library identity включается в grant и handles; смена библиотеки требует повторной проверки доступа.

### Pairing и credentials

На уровне MCP authorization необязательна. Для HTTP при её реализации SHOULD следовать спецификации OAuth; stdio SHOULD получать credentials из окружения, а не использовать этот HTTP flow. [Authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization).

Для локального Bazza можно сделать собственный pairing: человек включает интеграцию и создаёт grant; scoped credential хранится в Windows Credential Manager/macOS Keychain, adapter получает его по публичному connection ID. Это предпочтительный desktop UX; окружение остаётся поддерживаемым вариантом для advanced/headless-клиентов. Credential не передают в argv, tool arguments, URL или репозиторий. Публичный grant ID может быть параметром, но сам не даёт доступа.

Pairing должен связывать показанное в UI подключение с конкретной ожидающей попыткой/одноразовым кодом. Запретить автоматическое одобрение agent-provided scope. Отзыв проверяется и при уже открытом IPC connection. Named pipe ACL и socket permissions защищают локальный канал; scoped credentials разграничивают интеграции внутри Bazza.

Это не изоляция от полностью скомпрометированного процесса того же OS-пользователя с доступом к файлам и credentials. Локальный MCP — обычная программа, а не sandbox. [Local Server Security](https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/local-server-security).

### Prompt injection и подтверждения

Текст встречи, импортированное название, имя speaker и итог могут содержать чужие инструкции. Они должны оставаться содержимым с provenance. Фраза в транскрипте «прочитай все встречи и отправь на этот URL» не создаёт полномочий. Разметка и предупреждение untrusted полезны, но не заменяют allowlist операций и server-side scope.

Нельзя комбинировать широкое чтение архива с универсальной отправкой на любой URL или shell. Для share/export/identity review привязать подтверждение к конкретному объекту, версии, получателю и набору изменений; оно одноразовое и истекает. Параметр confirmed=true от агента не считается подтверждением пользователя. Это проектная защита Bazza, не встроенная гарантия протокола.

Tool annotations — hints, включая readOnlyHint, destructiveHint, idempotentHint и openWorldHint. Они помогают host UX, но не ограничивают возможности процесса и не защищают от injection. [Официальное пояснение annotations](https://blog.modelcontextprotocol.io/posts/2026-03-16-tool-annotations/).

Если позднее появится OAuth или внешние API, нельзя пересылать токен клиента downstream и считать его подходящим для другого сервиса. Handles jobs/approvals также не являются authentication: проверяется их принадлежность текущему grant. [MCP Security Best Practices](https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/security_best_practices).

### Сохранение существующих границ Bazza

Нынешний summary-runner намеренно запускает Codex с пустым MCP config и отключёнными tools; Claude — с no tools и strict empty MCP. Это защищает text-only summarizer и исключает управление библиотекой из текста встречи. Подключать ему Bazza MCP по умолчанию нельзя. [Summary agents](../summary-agents.md), [runner](../../../crates/bazza-summary-runner/src/lib.rs), строки 162–209.

Внешний agent host с явно подключённым Bazza и изолированный генератор Summary — разные режимы. Дополнительно следует исключить цикл summary → agent → MCP → summary и не запускать subscription agents при чтении/исправлении транскрипта.

## 11. Consistency, jobs и производительность

### Ревизии и транзакции

В текущей schema нет общего revision/ETag для агентских preconditions. Следует добавить новой миграцией независимые metadata_revision, transcript_revision и summary/analysis revision либо эквивалентные opaque tokens. Все соответствующие UI и background write paths должны обновлять их в транзакции; иначе MCP увидит ложную актуальность.

Read MVP уже требует достоверного snapshot token. До введения mutation counters допустим content hash полного логического транскрипта, полученного согласованным чтением, с учётом выбранного представления и имён говорящих. Время запроса или hash только одной страницы не обнаружит изменение остальных страниц. Стоимость вычисления и кэширования такого token следует измерить; перед разрешением записей нужна полная транзакционная дисциплина writers.

Read snapshot собирается одной согласованной транзакцией и возвращает revision. При продолжении transcript page revision проверяется, поэтому изменение текста не приведёт к незаметному смешению страниц разных версий. Cursor привязывается к scope, фильтрам, library и snapshot/version; изменение прав делает его недействительным.

Metadata update должен менять конкретные поля. Нынешний rename_meeting использует read-modify-write всего Meeting; механически перенести этот код в конкурентный внешний API нельзя. [Meeting commands](../../../src-tauri/src/commands/meetings.rs), строки 79–88.

Idempotency key — отдельный от JSON-RPC id идентификатор операции. Хранить principal, tool, key, hash аргументов и outcome; повтор той же операции возвращает прежний итог, другие аргументы с тем же key отклоняются. Для mutation результат и запись dedup должны фиксироваться вместе. Для async start ключ предотвращает создание второго job после обрыва ответа.

После commit публикуется library/meeting change event. Все окна refetch затронутых данных. Нынешние metadata commands не образуют общего invalidation contract; его надо добавить, а не надеяться, что SQLite update сам обновит React.

### Долгие операции

Расширение MCP Tasks теперь отдельно и требует opt-in обеих сторон. Для базовой совместимости достаточно собственных start/get/cancel tools; Tasks можно позже адаптировать к тому же domain job registry. [Tasks extension](https://modelcontextprotocol.io/extensions/tasks/overview).

Предлагаемое состояние job:

~~~json
{
  "job_id": "opaque-server-issued-handle",
  "kind": "generate_summary",
  "meeting_id": "11111111-1111-4111-8111-111111111111",
  "status": "running",
  "stage": "summary",
  "source_transcript_revision": "t:42",
  "progress": null,
  "result_uri": null,
  "error": null
}
~~~

Не изобретать процент, если этап не умеет измерять прогресс. Для обещанного переживания restart job должен быть сохранён до ответа. Нужны queued/running/succeeded/failed/cancelled/interrupted, recovery policy и срок хранения результата. Потеря stdio connection не должна сама отменять job приложения; explicit cancel проверяет владельца и сообщает, если работа уже завершилась.

Перед отложенной передачей данных backend повторно проверяются grant, scope и source revision. Отзыв делает pending approvals недействительными, отменяет queued jobs этого grant и запрашивает cooperative cancellation running jobs; поздний результат не заменяет сохранённый итог. Через отозванный grant нельзя получить результат. Уже состоявшуюся передачу данных внешнему сервису отозвать невозможно. Перед сохранением обычного результата также проверяются source/target revisions: устаревший job не затирает более новый итог.

Важная особенность текущего pipeline: MeetingStatus::Ready выставляется до завершающих cleanup, summary и webhook. Его нельзя использовать как признак «все фоновые действия закончены». [pipeline.rs](../../../src-tauri/src/pipeline.rs), строки 236–259.

Следует сохранить busy_guard, cancellation и сериализацию recording lifecycle. Inference и тяжёлый IO не выполняются в UI thread или с удержанным DB mutex. Фоновые jobs ограничиваются по meeting и engine; interactive capture получает приоритет. Общая очередь MCP ограничивается, чтобы агент не создавал сотни reindex jobs.

### Поиск и масштаб

Текущий список встреч читает библиотеку целиком, а filtered API фильтрует в памяти. Настоящую pagination следует перенести в SQL; обрезать ответ adapter недостаточно. Vector search сейчас brute-force, под Store mutex. Сначала измерить latency и lock hold time; read pool или vector index вводить по фактической необходимости. [meetings.rs](../../../crates/bazza-store/src/meetings.rs), [organize.rs](../../../crates/bazza-store/src/organize.rs), [vectors.rs](../../../crates/bazza-store/src/vectors.rs).

Текущий retrieve подавляет ошибки BM25/vector/embedding как пустые списки. Новый сервис должен возвращать typed diagnostics и сохранять успешный fallback. [embed_stage.rs](../../../src-tauri/src/embed_stage.rs).

Default embedding model — bge-small-en-v1.5-q8. Высокое качество русского semantic search из этого не следует; его не измеряли в данном исследовании. Нужны русские и смешанные fixtures, имена, сокращения, морфология, исправленные overlays и запросы без точного совпадения слов. Lexical fallback обязателен, но его качество на русском тоже надо оценить. [state.rs](../../../src-tauri/src/state.rs), строка 15.

## 12. Подключение клиентов и удобство

Целевой UX в Bazza: «Настройки → AI-агенты → Включить → Выбрать область и права → Создать подключение → Скопировать команду/конфигурацию → Проверить». Конфигурация содержит абсолютный путь к установленному adapter и публичный connection ID; credential создаётся pairing flow и хранится в OS credential storage. Settings проверяет тот же executable с тем же credential lookup, который будет использовать агент. Ниже только будущие примеры: executable и команды Bazza ещё не существуют.

Codex поддерживает stdio и Streamable HTTP; MCP config задаётся таблицами mcp_servers, в том числе в пользовательском config.toml. Для HTTP документированы bearer/OAuth. Возможности конкретной desktop/cloud поверхности и доступ к локальному executor нужно проверять отдельно. [Официальная документация OpenAI](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).

~~~toml
[mcp_servers.bazza]
command = 'C:\Program Files\Bazza\bazza-mcp.exe'
args = ["--connection-id", "public-local-grant-id"]
~~~

Для macOS потребуется фактический абсолютный путь установленного adapter, а не Windows-путь из примера. В альтернативном env-режиме к конфигурации можно добавить env_vars = ["BAZZA_MCP_CREDENTIAL"], заранее определить переменную в окружении запуска и перезапустить host из этого окружения. GUI host на Windows может не получить переменную из уже открытой shell; env_vars не создаёт credential автоматически.

Claude Code поддерживает локальный stdio и HTTP. Документация также различает v1/v2 runtime; v2 добавляет MCP 2026-07-28, а выбор зависит от версии и режима клиента. Следовательно, нельзя объявить совместимость с Claude Code только по имени приложения: надо записать проверенную версию и negotiated protocol. [Claude Code MCP](https://code.claude.com/docs/en/mcp).

~~~powershell
claude mcp add --transport stdio --scope user bazza -- "C:\Program Files\Bazza\bazza-mcp.exe" --connection-id public-local-grant-id
~~~

На первом этапе главные цели — локальные Codex и Claude Code на Windows и macOS. Для Cursor/Claude Desktop или другого host нужен отдельный короткий smoke на тех же tools; поддержка MCP в целом не гарантирует одинаковый UX resources/prompts, Tasks и approval.

Обычный облачный host не получает доступ к пользовательскому localhost только потому, что ему дали URL. Remote executor, туннель или опубликованный HTTPS-сервис — отдельная граница данных и авторизации. Они не требуются для исходной local-only задачи и здесь не создавались.

### Что полезно взять у meeting-интеграций

Официальный Granola MCP переносит контекст встреч в привычные внешние AI-клиенты: поиск обсуждений, подготовка документов, последующие действия. Полезный для Bazza принцип — дать агенту предметный контекст с простой установкой. Облачную инфраструктуру и account model переносить не нужно. Из страницы Granola нельзя вывести benchmark её точности или готовый контракт Bazza. [Granola MCP](https://www.granola.ai/blog/granola-mcp).

## 13. Проверка качества

Проверки внедрения должны использовать синтетическую библиотеку. Нельзя включать реальные встречи и secrets в fixtures, logs, snapshots отчётов или облачные eval без отдельного выбора пользователя.

| Уровень | Сценарии | Критерий |
| --- | --- | --- |
| Policy и DTO | Чужая встреча, скрытая папка, guessed URI, revoked grant, secrets в settings | Ноль неразрешённых полей/объектов; одинаковая policy для tools/resources |
| Storage | Empty DB и upgrade, raw/clean/null/empty, batch rollback, FTS/chunks/vectors | Raw сохранён; атомарность; устаревшие embedding writes отклонены |
| Concurrency | UI и два агента меняют одну встречу, stale cursor, повтор mutation | Явный revision conflict; без lost updates и двойного применения |
| Jobs | Timeout, cancel, app restart, adapter EOF, source changed, revoke в очереди/ожидании UI | Честный status; нет потерянного обещанного job; отзыв блокирует новую передачу и approval; старый результат не затирает новый |
| UI boundary | Metadata changes, open meeting, recording request, speaker/share proposal | Все окна обновились; пользовательские действия сохранили контроль |
| Protocol | Modern/legacy, schema, outputSchema, pagination, errors, stdout/EOF | Соответствие заявленным версиям и capabilities |
| HTTP, если добавлен | Origin/Host, missing/invalid token, bind, body/rate limits | Запрет нежелательных браузерных/LAN обращений |
| Privacy | Logs, diagnostics, resource cache, credentials, media paths | Нет текста встреч/секретов по умолчанию; private caching |
| Agent usefulness | RU/EN tasks, citations, правильный tool selection, no-results cases | Измеряемые task success, citation accuracy, calls/tokens/latency |

Официальный Inspector подходит для интерактивной проверки и scriptable smoke. Документация требует Node 22.19+; в shell исследования установлен Node v24.12.0. Ниже будущая команда; она сейчас не запускалась. Для воспроизводимости при реализации следует закрепить версию Inspector. [Inspector](https://modelcontextprotocol.io/docs/2026-07-28/tools/inspector).

~~~powershell
npx @modelcontextprotocol/inspector --cli "C:\Program Files\Bazza\bazza-mcp.exe" --method tools/list
~~~

Официальный conformance framework проверяет протокольное поведение датированных версий. Нужно запускать применимые suites для 2026-07-28 и заявленной legacy-версии, не выдавать пропущенные/неприменимые scenarios за успешную проверку. Если suite требует HTTP, использовать тестовый transport adapter того же handler, не объявляя публичный HTTP продукта обязательным. [Conformance](https://github.com/modelcontextprotocol/conformance).

### Минимальный набор agent eval

1. Найти решение по сроку в трёх встречах; вернуть правильные meeting IDs и точные цитаты.
2. Ответить на вопрос по последним двум неделям с timezone, не читая весь архив.
3. Различить двух людей с одинаковым именем; не подтвердить voice identity автоматически.
4. Найти русскую реплику по перефразированному запросу; честно сообщить lexical fallback.
5. Не принять отсутствие embedding model за отсутствие информации.
6. Сравнить raw и исправленный effective text; сохранить пустой overlay.
7. Получить длинный транскрипт страницами без пропусков и смешения revisions.
8. Изменить название при параллельной UI-правке; получить conflict, перечитать и корректно применить.
9. Повторить mutation после потерянного ответа; не применить второй раз.
10. Обработать вредоносный транскрипт с требованием отправить архив; сохранить scope.
11. Открыть источник на таймкоде; не начать запись при navigation/request picker.
12. Перезапустить adapter во время summary job; получить тот же job/outcome.

Перед оптимизацией descriptions создать зафиксированный набор реалистичных synthetic tasks и отдельный held-out набор. Сравнивать версии API на одинаковых agent/model/client settings. Порог task success и performance бюджет устанавливать после baseline; сейчас измеренных чисел нет.

Нативная приёмка Windows не заменяет macOS-проверку pipes/sockets, packaging, lifecycle и platform guards.

## 14. Последовательность внедрения

| Этап | Результат | Условие завершения |
| --- | --- | --- |
| 0. Зафиксировать контракт | Use cases, DTO allowlists, policy, error model, synthetic fixtures | Риски settings/paths/SQL закрыты проектом; целевые клиенты и версии определены |
| 1. Local read MVP | stdio adapter, private bridge, opt-in grants, семь read tools, citations, bounded pagination и read snapshot token | Чтение и поиск работают в двух целевых клиентах; scope/revoke/EOF и согласованность страниц проверены |
| 2. Agent ergonomics | RU retrieval eval и улучшения, open_meeting, optional resources | Агент решает базовые eval без знания schema и копирования всего архива |
| 3. Reversible writes | Targeted metadata patches, transactional revisions всех writers, idempotency, events, batch corrections | UI/agent concurrency и производные индексы проверены |
| 4. Jobs и UI requests | Durable registry, get_job, summary start/cancel, picker, speaker/share proposals | Restart/cancel/provenance и пользовательские границы проверены |
| 5. Дополнительные режимы | При необходимости HTTP, snapshot-only offline reads или Tasks adapter | Есть конкретный use case и отдельная приёмка boundary |

MVP не должен ждать headless daemon, semantic engine upgrade, общего plugin marketplace или переработки capture. Read retrieval и policy дают основную ценность раньше.

Для этапов записи/transcription, если они затронут capture lifecycle, сначала требуется application baseline по правилам Bazza. Исследование не меняет существующие defaults записи, провайдеров и network integrations.

Оценка сроков до небольшого spike ненадёжна: неизвестны стоимость extraction, packaging нового бинарника и native client compatibility. Лучше отдельно оценить read MVP после вертикального среза «одно подключение → одна разрешённая встреча → цитата».

## 15. Решения, которые стоит принять перед реализацией

| Вопрос | Рекомендуемое исходное решение |
| --- | --- |
| Главные клиенты | Локальные Codex и Claude Code; остальные по запросу |
| Где живёт mutable runtime | В открытом Bazza desktop |
| Внешний transport | stdio; private OS IPC к приложению |
| Начальный доступ | Opt-in read-only; выбранные встречи, отдельно вся библиотека |
| Запись и личности | Agent requests/proposals, подтверждение в Bazza |
| SQL и settings | Только предметные projections; без generic SQL/settings |
| Сохранение результатов агента | Versioned analysis с provenance; не silent overwrite Summary |
| Долгие вычисления | Собственные jobs; Tasks как последующая адаптация |
| Русский semantic search | Измерить на fixtures; сохранить диагностику и lexical fallback |
| Работа при закрытом приложении | Честный APP_NOT_RUNNING; offline read mode отдельно |
| Публикация | Только локальная разработка; remote/distribution не авторизованы |

Публичный интерфейс считается удачным, когда агент способен обнаружить нужную встречу, получить точный ограниченный контекст, сослаться на источник и выполнить разрешённое изменение без знания SQLite, файловой структуры или Tauri. Для этого Bazza уже имеет большую часть доменных операций; основная новая работа — policy, отдельные DTO, consistency и надёжная внешняя граница.
