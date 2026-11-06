# MCP: граница контракта и полномочий

Дата: 2 ноября 2026 года. Статус: нормативные решения этапа 0.

Документ фиксирует имена tools, область library grant и разделение ролей для
[плана реализации](../plans/2026-11-02-mcp-implementation.md). Требования M1–M11
и критерии завершения принадлежат плану. Здесь не объявляется реализация MCP,
готовность production service или выполнение native acceptance.

Исполняемые JSON Schema всех 29 базовых tools находятся в
[bazza-service-api](../../../crates/bazza-service-api/src/lib.rs). Они описывают
будущий публичный контракт. Каталог схем не является фактическим `tools/list`
сервиса и не объявляет handlers или runtime capabilities реализованными.

## Имена публичных tools

Имена базового набора закреплены без алиасов. Порядок `tools/list` должен быть
стабильным; конкретная реализация публикует только поддерживаемые capabilities.

| Группа | Закреплённые имена |
| --- | --- |
| Состояние | `bazza_get_status` |
| Библиотека | `bazza_list_meetings`, `bazza_get_meeting`, `bazza_get_library_structure` |
| Встречи | `bazza_create_meeting`, `bazza_update_meeting`, `bazza_delete_meeting` |
| Транскрипт | `bazza_get_transcript`, `bazza_import_transcript`, `bazza_replace_transcript`, `bazza_set_transcript_corrections`, `bazza_delete_transcript` |
| Саммари | `bazza_get_summary`, `bazza_write_summary`, `bazza_delete_summary` |
| Заметки | `bazza_list_notes`, `bazza_get_note`, `bazza_create_note`, `bazza_update_note`, `bazza_delete_note` |
| Поиск | `bazza_search` |
| Календарь | `bazza_list_calendar_events`, `bazza_refresh_calendar` |
| Sharing | `bazza_list_shares`, `bazza_create_share`, `bazza_update_share`, `bazza_delete_share` |
| Jobs | `bazza_get_job`, `bazza_cancel_job` |

Для отдельного этапа 9 зарезервированы `bazza_list_shared_meetings`,
`bazza_get_shared_meeting`, `bazza_refresh_shared_source`. Они не входят в
объявление готовности базового набора. Library grant не расширяет права,
выданные владельцем удалённого источника. Refresh использует зарегистрированные
совместимые sources; произвольный HTTP fetch не становится публичным tool.

Новых публичных tools для capture, private settings, voiceprints, SQL, shell,
произвольных файлов или универсального `invoke(method,args)` нет. Создание
карточки встречи не начинает запись. Календарь ограничен cache и refresh уже
настроенных ICS-источников; grant не разрешает CRUD Google/Outlook events.

## Роли private RPC

Роль устанавливает сервис по аутентифицированному соединению и проверенным
полномочиям. Поле запроса, имя метода или известный объектный ID не повышают роль.
Public connection ID используется для выбора подключения, а не для проверки
его подлинности.

| Возможность | External MCP principal | Application client | Registered worker | Administration |
| --- | --- | --- | --- | --- |
| Продуктовые чтение/CRUD/search/sharing | Только через публичный domain API и разрешённые операции grant | Через typed application API | Только назначенные input snapshots и результат своей операции | Административная роль не подменяет отдельный application/external contract |
| Private settings и выбранные пользователем интеграции | Нет | Только соответствующие пользовательские application-команды | Только минимальные данные назначенной операции; нет общего settings API | Только конкретные административные операции |
| Capture/inference и публикация worker result | Нет | Управление существующими пользовательскими сценариями | Зарегистрированная session, capability, lease и fencing token | Нет обхода worker lease |
| Pairing, список и revoke grants | Нет | UI обращается к соответствующему административному flow | Нет | Да, включая локальный CLI без GUI |
| Library migration/relocation и service lifecycle | Нет | Только определённый application/control flow | Нет | Typed control operations; сервис исполняет изменение библиотеки |
| Прямой production Store/SQL и managed writes | Нет | Нет после GUI cutover | Только назначенный raw/staged artifact в пределах lease | Store и публикацией файлов владеет сервис; generic SQL не экспортируется |

Application trust не наследуется external credential. Административные и worker
методы не включаются в MCP tools/resources. Известный job ID не позволяет вызвать
worker publish или отменить capture, recovery и обязательный purge.

Конкретное соответствие tool → operation permission закреплено в
[typed permission catalog](../../../crates/bazza-service-api/src/permissions.rs).
Get/list/search требуют `read`; создание встречи/заметки — `create`; изменения
metadata, transcript и notes, запись summary, calendar refresh и запрос отмены
job — `update`; удаление встречи/контента — `delete`; все share mutations —
`share`. Summary `mode=create` изменяет существующий summary state встречи и
тоже требует `update`. `list_shares(include_url=true)` дополнительно требует
`share`. Отмена job также проверяет исходные полномочия и разрешённый public job
kind. Эта таблица описывает policy; сам grant checker реализуется в data core.

Сервис остаётся единственным владельцем production Store. GUI не открывает
локальный SQLite fallback при потере сервиса. Capture callbacks остаются bounded
и не делают RPC; условия artifact ownership, handoff и recovery определены
этапом 3 плана и проверяются до production cutover.

## Один grant на библиотеку

Человек один раз разрешает выбранному principal набор операций чтения, создания,
изменения, удаления и управления sharing выбранной библиотеки. Область охватывает
всю текущую и будущую библиотеку. Ни allowlist встреч, ни отдельного consent для
каждой встречи, ни `confirmed=true`, ни UI approval queue для уже разрешённых
mutations сервер не вводит. Выбранные операции проверяются сервисом на каждом
запросе и перед отложенным side effect.

Grant содержит library ID, principal/client identity, operation permissions,
created/revoked state и ссылку на credential. Pairing хранит credential в Windows
Credential Manager/macOS Keychain; обычная конфигурация содержит public connection
ID. Advanced lookup через окружение требует той же проверки прав. Секрет не
становится частью DTO, URI или диагностического сообщения.

Разрешение локальной библиотеки не отменяет политики approval самого agent host.
UI объясняет, что возвращённые данные получает выбранный host и он может
обрабатывать их внешним провайдером. У этой защиты нет обещания изолировать
полностью скомпрометированный процесс того же OS-пользователя.

## Revoke и отложенные эффекты

Revoke имеет общую точку сериализации с проверкой grant/permission epoch и commit
mutation либо активацией share. Проверки только в adapter или до начала долгой
операции недостаточно. После committed revoke новый внешний эффект не начинается.
Queued external operations без commit отменяются; running получают cancellation
intent, а фактическое завершение отражается отдельным terminal outcome.

Уже committed изменения сохраняются. Обязательный cleanup/purge продолжает
сервис; отзыв клиента не оставляет логически удалённые данные опубликованными.
Уже переданные данные вернуть невозможно. По умолчанию revoke также отключает
созданные подключением active/preparing shares; локальный admin может явно
сохранить конкретные shares. Открытое соединение не сохраняет прежние полномочия
после revoke.

## Revisions, ABA и повтор запроса

Metadata, transcript state, summary state, каждая note, share и lifecycle state
имеют отдельные opaque revisions. Изменение значения, увеличение revision,
change event и минимальный idempotency outcome фиксируются одной транзакцией.
Update/delete требуют `expected_revision`; связанные metadata меняются typed
patch и предметными add/remove tag operations, без full-row read-modify-write.

Transcript и summary сохраняют state revision даже при отсутствии либо после
удаления документа. Read возвращает эту revision. Create/replace/delete сверяет
её, поэтому последовательность absent → create → delete не возвращает прежнее
состояние для устаревшего writer (ABA). `bazza_write_summary` с `mode=create`
требует absent current, с `mode=replace` — существующий current; оба сверяют
текущую summary-state revision. Поздний worker сверяет source/target generations
и не восстанавливает явно удалённый transcript/summary.

Каждый write/delete/start-job tool принимает `idempotency_key`. Его scope:
principal + library + tool + hash нормализованных аргументов. Тот же key/payload
возвращает прежний outcome, другой payload — `IDEMPOTENCY_CONFLICT`. JSON-RPC ID
остаётся correlation ID и не заменяет key. Dedup record хранит IDs/revisions и
минимальный outcome, не копию удаляемых body. Повтор не обходит проверку текущих
полномочий.

Целевой dedup retention — семь суток с фиксации первого durable outcome; повтор
не продлевает этот срок. Receipt содержит `dedup_expires_at`, а
`DEDUP_RETENTION_SECONDS` задаёт 604800 секунд. Для start-job первым outcome
является durable регистрация job, а не окончание её исполнения. После истечения
срока гарантия прежнего outcome не обещается. Это контракт будущего data core,
а не уже работающая гарантия: durable records и их очистка относятся к этапу 2.

## DTO, ошибки и ограничения размера

Идентификаторы библиотеки/встречи/документа/job и resource URI — предметные ссылки,
не filesystem paths. Публичные DTO не возвращают абсолютные media paths, private
calendar URLs, credentials, voiceprints или bearer tokens. Исключение для share
URL возможно только у операции, которая явно возвращает разрешённую ссылку.
Resources используют тот же domain API и ту же authorization policy, что tools.

Input/output — object-shaped JSON Schema 2020-12. Input запрещает неизвестные
поля через `additionalProperties=false`; строки, массивы, mutation batches и
общие байты запроса ограничены. Typed envelopes разделяют correlation/request ID,
auth context и deadline. Frames и очереди private RPC также bounded. Их точные
числовые бюджеты проверяются и фиксируются в исполняемом контракте этапа 1.

Output envelope содержит `schema_version`, `library_id`, `data` либо `error`,
`warnings`. Output schema допускает domain error; MCP возвращает его с
`isError=true`, а protocol errors обрабатывает SDK. Error содержит безопасные
`code`, `message`, `retryable` и при необходимости `retry_after_ms` либо
`current_revision`; stack trace, SQL, paths и provider secrets в него не попадают.
Набор кодов принадлежит разделу 4 плана.

Закреплённые начальные лимиты: search default 20/max 100, transcript page
не более 100 segments или 64 KiB, note/summary page до 64 KiB. Слишком длинный
segment получает text continuation. UTF-8 не разрывается, продолжение/truncation
явно описывается cursor metadata. Request и response ограничены 1 MiB
сериализованного JSON каждый; document mutation — 256 KiB UTF-8, один segment
или correction fragment — 64 KiB UTF-8, transcript mutation — 1000 segments,
atomic correction batch — 100 операций. Arrays участников ограничены 64,
обычные страницы и массивы IDs — 100 элементов. Это проверяемые исходные
budgets; изменение требует согласованного обновления контракта и tests.

Текст встречи сохраняется как исходный raw; обычная коррекция меняет только
cleaned overlay, включая намеренно пустой. Import/replace/delete transcript —
отдельные явные операции с state revision. Базовая create meeting принимает
готовые timed segments и не фабрикует таймкоды для plain text. Удаление встречи
при active capture возвращает `RESOURCE_BUSY` и не вызывает скрытый Stop.

## Оставшаяся работа контракта

Схемы, permission map и retention target зафиксированы. До объявления публичного
API готовым нужны handlers, service-side grant enforcement, transactional data
core, protocol compatibility tests, revoke/commit race tests и native acceptance
M11. MCP writes не подключаются к рабочей библиотеке до schema/GUI cutover gates.

## Исполняемые схемы и validation

[Typed inputs](../../../crates/bazza-service-api/src/inputs.rs) и
[public output projections](../../../crates/bazza-service-api/src/outputs.rs)
генерируют self-contained JSON Schema 2020-12 через закреплённый Schemars 1.2.2.
Inputs и вложенные предметные объекты запрещают неизвестные поля. Success/error
ветки output envelope взаимоисключающие. Выходные nullable поля сохраняются
явными, включая revision absent/deleted summary/transcript state.

`tool_schemas()` возвращает стабильный каталог; `validate_tool_input()` и
`validate_tool_output()` используют настоящий Draft 2020-12 validator
jsonschema 0.56.0 с включённой проверкой formats. Его HTTP/file resolvers
отключены в зависимостях; схемы не требуют внешних references. После schema
validation проверяются сравнение времён, уникальность correction/tag targets,
непустые typed patches и UTF-8 byte budgets. JSON Schema `maxLength` считает
Unicode characters, поэтому не заменяет эту проверку байтов.

`validate_tool_output_for_input()` дополнительно сверяет requested limit и
max_bytes, representation/format и явный `include_url`. Handlers должны применять
эти проверки вместе с domain authorization. `slice_utf8_page()` разрезает текст
по границе UTF-8, проверяет offset и отказывает, если budget не вмещает даже
следующий символ; cursor не застревает на пустом фрагменте.

Summary `structured_json` передаётся ограниченной строкой с encoded JSON, чтобы
сохранять произвольные legacy structures без неограниченного generic object.
Input проверяется как JSON; чтение выбирает paged `markdown` либо
`structured_json`. Provenance для write summary назначает сервис: допустимы
`manual`/`external_agent` и `auto_replace=false`. Caller не выбирает legacy или
configured-generation identity. Transcript source projection содержит только
безопасные origin/provider/model labels и language, без private settings.

Экспорт для review: `cargo run -p bazza-service-api --example export_schemas`.
Он выводит JSON с `contract_only=true` и пустым `implemented_capabilities`, а не
работает как MCP transport. Adversarial checks находятся в
[contracts.rs](../../../crates/bazza-service-api/tests/contracts.rs):
`cargo test -p bazza-service-api --test contracts`. Фактические результаты
запусков фиксирует report/план интегратора.

API генерации/validation сверены с первичными описаниями
[Schemars](https://docs.rs/schemars/1.2.2/schemars/generate/struct.SchemaSettings.html)
и [jsonschema](https://docs.rs/jsonschema/0.56.0/jsonschema/draft202012/index.html).
