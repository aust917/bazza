# План реализации системной диктовки Bazza

Статус: active — план подготовлен; реализация не начата.
Дата: 2 ноября 2026 года.
Владелец: интегрирующая сессия диктовки; отдельная ветка не создавалась. Перед реализацией согласовать владение общими файлами с параллельными сессиями: рабочее дерево уже содержит их изменения.

[Исследование и GitHub-референсы](../research/2026-11-02-system-dictation.md) · [Архитектура](../architecture.md) · [STT](../stt.md) · [Правила работы](../agent-workflow.md) · [Проверки](../testing.md)

## 1. Результат и границы

Пользователь ставит курсор в поле другого приложения, запускает диктовку глобальной клавишей, произносит текст и завершает запись. Bazza распознаёт только микрофон выбранной локальной или облачной моделью и делает одну автоматическую попытку вставки окончательного текста. Индикатор показывает состояние и позволяет остановить/отменить диктовку. При смене цели или неудаче текст доступен для Copy.

Обязательный результат: Windows и macOS, настраиваемые hold-to-talk и toggle, локальный Whisper и opt-in Soniox, независимый от встреч выбор модели, корректная отмена и восстановление текста. Сначала закрывается Windows-срез; готовность macOS подтверждается отдельно на реальном Mac.

Функция работает при закрытом основном окне, пока desktop-процесс Bazza остаётся запущенным. Завершённый процесс не обслуживает hotkeys; обязательный autostart не добавляется. Диктовка не создаёт Meeting, записей в библиотеке, summary/diarization/embeddings/webhook и не захватывает системный звук.

В первый обязательный объём не входят другие ASR engines/providers, LLM-редактура, чтение контекста документа, история/аудио на диске, голосовые команды, TSF/IME composition, одновременная встреча и диктовка, Linux и App Store sandbox. Дополнения перечислены в конце; они не задерживают готовность базовой диктовки.

Все изменения локальные: без push/PR/публикации и изменения release identity/updater settings.

## 2. Проектные решения

1. **Отдельная сессия.** `DictationSession` владеет микрофоном, bounded PCM, ASR worker и отменой. Meeting recorder и его mixed PCM не используются.
2. **Независимый выбор.** `dictation.provider/model/language` не меняют STT встреч. Local — значение по умолчанию; Soniox требует отдельного согласия на диктовку и доступного ключа.
3. **Один final insert.** Partial text показывается только в индикаторе. Raw результат сохраняется отдельно от будущего formatted; без редактора значения совпадают.
4. **Исходное поле.** Сохраняется identity поля до показа индикатора; вставка выполняется в то же поле по его текущему caret/selection. При смене поля/вкладки/окна — recovery. Для неизвестной identity auto-paste разрешается только проверенным app adapter, иначе Copy.
5. **Одна попытка со стороны Bazza.** `insertion_id` блокирует автоматические повторы. Dispatch и Cancel сериализуются общей точкой перехода; после отправки события ОС отмена не обещает откат текста.
6. **Явные исходы.** Различать `failed_before_dispatch`, `dispatched_unverified`, `target_changed`, `blocked`, `cancelled`; `confirmed` использовать только при достаточном app-specific evidence. Clipboard receipt не подтверждает вставку.
7. **Ограниченные ресурсы.** Начальный лимит фразы — 120 s, PCM mono f32 16 kHz — около 7,7 MB без учёта модели. На переполнение/потерю устройства — контролируемая ошибка, без бесконечной очереди и тихого пропуска аудио.
8. **Временное восстановление.** Последний final text хранить только в памяти, начальный TTL — 10 минут, очистка также при Quit. PCM очищать после фактического завершения использующего его worker. Сохранение аудио и повторное распознавание прежней фразы — отдельное будущее дополнение.
9. **Общий admission gate.** Встреча и диктовка взаимно исключаются в первом объёме. Reservation действует до завершения finalization/insertion/clipboard cleanup; recoverable текст остаётся доступен после освобождения ресурсов. Начатую встречу не прерывать ради диктовки.
10. **Отдельный desktop runtime.** Hotkeys, mic, engines, overlay и text injection остаются в desktop GUI. Диктовка не durable library job и не MCP tool. Новые Tauri-free компоненты не зависят от Store или headless service.
11. **Терминалы — отдельный профиль.** Terminal auto-insert по умолчанию выключен; доступен Copy. Разрешение конкретного terminal adapter требует проверки multiline/bracketed paste: перенос из самого текста может выполнить команду даже без добавленного Bazza Enter.

Численные лимиты выше — начальные проектные значения, проверяемые на этапе измерений. Они не являются измеренными показателями производительности.

## 3. Архитектура и точки изменений

| Область | Новые или изменяемые файлы | Ответственность |
| --- | --- | --- |
| Pure session core | Новый `crates/bazza-dictation/` | State machine, IDs/generation, bounded buffers, выбор провайдера, cancellation, итоговые DTO |
| Native input | Новый `crates/bazza-text-input/` с Windows/macOS adapters | Target snapshot/validation, clipboard transaction, no-duplicate dispatch, outcomes |
| Desktop orchestration | Новые `src-tauri/src/dictation.rs`, `src-tauri/src/commands/dictation.rs` | Mic/ASR lifecycle, settings snapshot, разрешения, ресурсы, соединение core и input |
| Shared desktop state | [state.rs](../../../src-tauri/src/state.rs), [lib.rs](../../../src-tauri/src/lib.rs), [processing_jobs.rs](../../../src-tauri/src/processing_jobs.rs) | Admission, engine lifecycle, scheduling, shutdown |
| ASR adapters | [bazza-asr](../../../crates/bazza-asr/src/lib.rs), [stt_stage.rs](../../../src-tauri/src/stt_stage.rs) | Повторное использование Whisper/Soniox без meeting sinks; session-aware cloud cancellation |
| Capture | [stream.rs](../../../crates/bazza-capture/src/stream.rs), [devices.rs](../../../crates/bazza-capture/src/devices.rs) | Mic-only stream, явная политика отсутствующего fixed device |
| Настройки | [settings.rs](../../../src-tauri/src/settings.rs), [commands/settings.rs](../../../src-tauri/src/commands/settings.rs) | Additive defaults, credential reference, consent/revocation по назначению |
| Hotkeys/windows | [shortcuts.rs](../../../src-tauri/src/shortcuts.rs), [windows.rs](../../../src-tauri/src/windows.rs), [tray.rs](../../../src-tauri/src/tray.rs), [tauri.conf.json](../../../src-tauri/tauri.conf.json) | Registry shortcuts, hold/toggle, nonactivating overlay, tray recovery |
| UI/IPC | Новый `app/src/windows/DictationBar.tsx`; [App.tsx](../../../app/src/App.tsx), [Settings.tsx](../../../app/src/routes/Settings.tsx), [ipc.ts](../../../app/src/lib/ipc.ts), [mock.ts](../../../app/src/lib/mock.ts) | Независимое окно, настройки диктовки, typed commands/events и mock |
| Разрешения/i18n | [capabilities](../../../src-tauri/capabilities/default.json), [Info.plist](../../../src-tauri/Info.plist), [UI locales](../../../app/src/locales), [native locales](../../../src-tauri/locales) | Минимальные capabilities, назначение микрофона, RU/EN строки и native error codes |
| Library lifecycle | [commands/organize.rs](../../../src-tauri/src/commands/organize.rs) | Учитывать новую сессию при `change_data_dir` и других изменениях пути/ресурсов |

Сначала зафиксировать общие DTO; затем Windows и macOS input adapters можно писать параллельно. Один владелец меняет `state.rs`, `lib.rs`, settings/IPC и registry. UI и отдельные adapters получают ограниченные задачи после фиксации контракта.

Соседняя MCP-работа уже добавляет `bazza-core`/`service-*`, но на дату плана production GUI ещё открывает Store напрямую; новые service crates нельзя считать готовым runtime диктовки. Settings/model paths/credentials поступают через desktop adapter. Если за время реализации GUI перейдёт на RepositoryClient, использовать актуальный путь настройки/данных; не добавлять второй Store opener или прямой fallback. Этот переход не должен блокировать pure dictation/input работу.

## 4. Контракты

Предполагаемые команды; финальные имена и сериализацию зафиксировать на этапе 1 одновременно в Rust, TypeScript и mock:

```text
dictation_start(mode) -> { session_id }
dictation_stop(session_id) -> { accepted }
dictation_cancel(session_id) -> { cancelling | too_late | already_finished }
dictation_get_state() -> snapshot
dictation_get_result(session_id) -> result | expired
dictation_copy_result(session_id) -> outcome
dictation_insert_result(session_id) -> outcome  // явная вставка в проверенную внешнюю цель

dictation-state: session_id, generation, phase, provider, model, error_code
dictation-preview: session_id, text, is_final
dictation-result: session_id, insertion_id, raw_text, formatted_text, outcome
```

`accepted` означает приём операции, а не завершение ASR. Повторные Stop/Cancel идемпотентны; callback прежней generation игнорируется. Явная повторная вставка получает новый attempt ID и не запускается автоматически при таймауте. Пользователь возвращает caret во внешнее поле и вызывает отдельный insert-last hotkey; Rust фиксирует и проверяет новую внешнюю цель. Окна самого Bazza целью не считаются. В UI основного окна достаточно Copy и инструкции о hotkey; клик по UI не разрешает вставку в случайное foreground окно. Copy — отдельная пользовательская операция записи в clipboard.

Состояния: `idle → starting → listening → finalizing → ready_to_insert → inserting → cleanup → completed/recoverable`; до dispatch допустим путь `cancelling → cleanup → cancelled`. Mic закрывается сразу, как только больше не нужен; worker/busy guard освобождаются после фактического выхода inference/network worker. Reservation снимается только после этого и ограниченного по времени restore-or-abandon clipboard cleanup с ownership guard. Хранение последнего текста не удерживает микрофон или reservation.

Существующий локальный `state.full(...)` синхронен и wrapper не предоставляет готовой немедленной остановки inference. Cancel сразу запрещает будущий dispatch, но UI остаётся в cancelling, пока worker не освободил используемые ресурсы. Нельзя разрешить новый конфликтующий Start или library switch лишь по факту нажатия Cancel. Quit использует ограниченное ожидание cleanup, затем штатное завершение процесса; не добавлять небезопасное убийство отдельных in-process threads и автоматическое возобновление после restart.

Начальные настройки: `enabled=false`, `provider=local`, `local_model_id`, `soniox_model`, `language`, `mic_device`, `mode=toggle`, отдельные hold/toggle shortcuts, `cloud_audio_consent=false`. Модель выбирается из совместимого установленного каталога; для облачной-only работы локальная модель не требуется. Ненайденная выбранная локальная модель даёт actionable ошибку/переход к загрузке, без подмены провайдера.

Обычные изменения model/language применяются к следующей сессии. Переключение active cloud диктовки в Local или отзыв dictation consent отменяет её сетевую работу без запуска другого backend посреди фразы. Удаление общего Soniox key отменяет все использующие его cloud sessions. Регистрация worker и revocation согласуются одним gate; уже начавшуюся отправку нельзя отозвать задним числом. Credential не возвращается через Settings/events, не логируется и не попадает в exports.

`enabled=false` снимает shortcuts диктовки, закрывает её mic/cloud input и запрещает ещё не dispatched ввод, сохраняя фактические resource guards до cleanup. Встречу это не отменяет. Повторное включение не возобновляет прежнюю сессию автоматически.

## 5. Этапы реализации

### Этап 0. Baseline и подготовка

- [ ] Повторно проверить `git status`, scoped instructions и актуальные ownership общих файлов. Не сбрасывать чужие изменения.
- [ ] Зафиксировать текущую сборку/конфигурацию и воспроизводимый meeting baseline на Windows: старт, запись, Stop, готовые media/transcript, существующий hotkey. Использовать disposable library и подходящие fixtures; описание процедур — в [testing](../testing.md).
- [ ] Уточнить актуальное состояние service migration и engine scheduling перед изменением AppState.
- [ ] Подготовить fixtures и оболочку insertion harness с фиксированными RU/EN/emoji строками без ASR/микрофона; определить журнал технических исходов без пользовательского текста. Native dispatch подключается на этапе 2 после фиксации контрактов.
- [ ] Зафиксировать необходимые зависимости/лицензии. Handy и OpenWhispr использовать по закреплённым в исследовании ревизиям; debug-gated `reliable_paste` не считать готовой гарантией. Сохранить notices при заимствовании.

**Gate:** baseline записан как фактические результаты; следующие изменения можно сравнить с ним. Harness не создаёт Meeting и не открывает аудиоустройства.

### Этап 1. Core, контракты и координация

- [ ] Создать pure state machine и input interface; закрепить DTO/ошибки/outcomes из раздела 4.
- [ ] Добавить nested `DictationSettings` с additive serde defaults/persistence, local model/mic/mode/enabled/shortcuts. Старые настройки остаются читаемыми; cloud UI и consent подключаются на этапе 4.
- [ ] Реализовать `session_id`, generation, attempt ledger и сериализованный Cancel/dispatch transition. Не держать общие locks во время ASR/сетевого ожидания.
- [ ] Добавить общий атомарный admission gate для meeting/dictation Start, длительной finalization, library switch и shutdown.
- [ ] Переработать hotkey registry: убрать глобальное `unregister_all()` при смене одного сочетания; регистрировать по назначению, с rollback при конфликте.
- [ ] Заменить singleton cloud cancellation slot session/purpose-aware registry, сохранив существующую meeting revocation semantics и контроль регистрации.
- [ ] В первом объёме при уже исполняемой несовместимой тяжёлой job отказывать в Start до открытия mic; новые конфликтующие jobs ждут завершения dictation reservation. Проверка и reservation каждой job должны использовать общий admission, включая весь runtime задачи. Захват не блокирует UI; preemption существующего inference не обещать.
- [ ] Определить owner thread native mic/COM handles, в особенности CPAL на macOS: в AppState хранить переносимый control handle/channel, не предполагать `Send` для native stream. Добавить отдельную true Exit policy; close-to-tray продолжает обслуживать функцию.
- [ ] Добавить regression tests для переходов, двойного Start/Stop, позднего callback, cancel race, admission и revocation; проверить meeting hotkey после изменения dictation hotkey и наоборот.

**Gate:** pure tests и targeted coordination tests проходят; существующая запись/облако не меняют выбранного провайдера и не теряют worker из-за новой регистрации.

### Этап 2. Windows: системная вставка без ASR

- [ ] Реализовать snapshot/validation target точнее PID/foreground HWND; учитывать поле/вкладку, закрытие и невалидные handles.
- [ ] Создать nonactivating indicator. Проверить создание, Show, обновление и клик Stop/Cancel, включая WebView child focus, повторный показ и multi-monitor.
- [ ] Реализовать clipboard transaction: bounded snapshot поддержанных форматов, markers, sequence guard, ограниченные ожидания, ownership-aware restore. При неудачном snapshot не очищать исходный clipboard молча.
- [ ] Добавлять Windows history/cloud exclusion formats; не обещать защиту от любого стороннего clipboard manager.
- [ ] Выбирать путь вставки до dispatch: clipboard paste либо direct Unicode для проверенного adapter. После отправки не запускать второй автоматический метод.
- [ ] Непосредственно перед dispatch повторно проверять цель, modifiers, cancel/generation под session gate. Password/read-only/elevated/unknown targets обрабатываются явным outcome/recovery.
- [ ] Проверить фиксированный текст в Notepad, Word, Chrome/Edge input/textarea/contenteditable и VS Code: пустое поле, caret, selection, RU/EN/emoji, абзацы, Undo.
- [ ] Проверить чужой Copy во время transaction, HTML/RTF/image/file snapshot, delayed clipboard owner, отсутствие добавленного Enter/отправки сообщения. Terminal/RDP профили учитывать отдельно.

**Gate:** в согласованной матрице нет наблюдаемых wrong-target/duplicate/clipboard-loss случаев; unverified исход сохраняет текст для ручного действия. Результаты фиксируются по приложению и способу ввода, не распространяются на все поля ОС.

### Этап 3. Windows: локальная диктовка

- [ ] Подключить отдельный `open_microphone_timed` с PCM 16 kHz mono. Установить лимит 120 s, bounded channel и гарантированное закрытие stream при Stop/Cancel/error/Quit.
- [ ] Определить явное поведение при недоступном fixed mic; не наследовать silent default fallback без уведомления.
- [ ] Использовать кэшированный `WhisperEngine::transcribe_opts` после Stop; не переносить meeting LiveTranscriber energy/min-duration policy на короткие фразы. Сохранять worker reservation до выхода синхронного inference даже при уже принятой отмене.
- [ ] Защитить inference `busy_guard`, устранить повторные cold-load одного model ID через singleflight, связать idle unload с новой сессией.
- [ ] Сначала подключить toggle через Rust hotkey dispatcher. Индикатор и callbacks не зависят от mount главного окна.
- [ ] Добавить local settings UI и RU/EN состояния listening/processing/cancelling/result/recovery, Copy и insert-last hotkey; TTL текста и очистку PCM после worker exit.
- [ ] Проверить «да»/«нет», тихую речь, тишину, имена/числа, смешанный RU/EN, лимит фразы, device removal и stale inference result после Cancel.

**Gate:** end-to-end local диктовка проходит Windows-матрицу; не создаёт встреч/файлов и не инициирует cloud ASR/cleanup. Сеть других явно включённых интеграций учитывается отдельно. Meeting baseline после изменения сохранён.

### Этап 4. Выбор local/cloud и Soniox

- [ ] Расширить независимый блок DictationSettings облачными полями и UI выбора local/cloud/consent/key status; сохранить additive defaults и совместимость старых настроек/библиотек.
- [ ] Подключить SonioxWorker к отдельному session sink, без `stt:{meeting_id}`/meeting captions/LiveFeed.
- [ ] Проверять dictation consent, credential presence и model ID до открытия cloud stream. Общий сохранённый ключ не означает согласия на диктовку.
- [ ] Использовать отдельное соединение на сессию: отправить весь принятый PCM, EOS, дождаться валидного terminal completion; partial tail не повышать до final по таймауту.
- [ ] Реализовать cancel/revoke/clear-key во время starting/listening/finalizing, bounded retry/timeouts, отказ на backlog/невалидный protocol.
- [ ] Проверить auth/quota/network/finalization ошибки; сохранить доступный final text с указанием неполноты. Не менять backend автоматически после ошибки.
- [ ] Проверить разные настройки встреч/диктовки, отсутствие взаимной отмены чужого purpose при обычной регистрации, отсутствие ключей/аудио в логах и events.

**Gate:** пользователь получает один и тот же UX вставки с выбранным Whisper или Soniox; local/cloud choice диктовки не меняет настройки встреч. Отзыв dictation consent прекращает только разрешённую ему область; удаление общего ключа прекращает все использующие его сессии.

### Этап 5. Hold-to-talk и полный Windows-срез

- [ ] Добавить Pressed/Released, подавление autorepeat, debounce и idempotent Stop. Обычный chord с клавишей; Fn-only/modifier-only backend не входит в базовый объём.
- [ ] Обработать потерянный release, смену комбинации, sleep/lock/wake, закрытие индикатора и Quit; предусмотреть доступные Stop/Cancel и лимит длительности.
- [ ] Закрыть настройки, shortcuts conflict UX, tray Copy/доступ к последнему результату, listener cleanup и session filtering всех окон.
- [ ] Измерить cold/warm `key_down→mic_ready`, `stop→ASR final`, `stop→dispatch` p50/p95; сохранить model/backend/устройство и условия сети. Назначить целевые задержки по этим данным и исправить выявленные задержки без изменения capture defaults.
- [ ] Повторить native-матрицу и meeting baseline на итоговой Windows-сборке.

**Gate:** Windows feature-complete: local/cloud, hold/toggle, отмена, recovery, работа с закрытым главным окном. Это промежуточная готовность, macOS ещё не считается принятой.

### Этап 6. macOS

Этап можно начинать параллельно с Windows после фиксации контрактов этапа 1. Он обязателен для общей готовности функции.

- [ ] Реализовать AX target identity и capability checks, CGEvent/NSPasteboard delivery, changeCount/marker guard и snapshot поддержанных форматов. Не использовать AXValue как замену всего содержимого ради вставки.
- [ ] Добавить nonactivating panel/подходящий adapter; проверить управление индикатором без изменения поля, fullscreen/Spaces и multi-monitor.
- [ ] Обновить microphone usage description и permission UX; Accessibility/Input Monitoring запрашивать по потребностям реального backend.
- [ ] Проверить revoked/denied права, Secure Event Input, пропущенный key-up, sleep/lock/wake и завершение приложения.
- [ ] Повторить synthetic и end-to-end матрицы в TextEdit/Safari/Chrome/VS Code с Whisper и Soniox, hold/toggle и RU/EN/emoji.
- [ ] Документировать Universal Clipboard как отдельную границу; быстрый restore не считать гарантией отсутствия синхронизации.
- [ ] Проверить запись встречи и зависимости/packaging на Mac. Если Mac недоступен, оставить gate открытым с точным перечнем непроверенного.

**Gate:** реальные native результаты macOS зафиксированы отдельно; готовность не выводится из Windows-тестов или cross-compile.

### Этап 7. Приёмка и документация

- [ ] Свести Windows/macOS результаты: поле/приложение/способ доставки, model/backend, ошибки, cold/warm latency, ограничения.
- [ ] Проверить upgrade существующих Settings, включение/выключение функции, library switch, shutdown и отсутствие захвата до пользовательского Start.
- [ ] Убедиться, что Cancel до dispatch, смена поля и поздний cloud final не приводят к автоматическому вводу; после dispatch UI честно отражает неопределённость/too_late.
- [ ] Зафиксировать отсутствие implicit submit, скрытого local→cloud fallback, Meeting/files/webhook side effects и перезаписи новой пользовательской копии.
- [ ] Обновить [архитектуру](../architecture.md), [STT](../stt.md), [privacy](../../privacy.md), [testing](../testing.md), [implementation status](../implementation-status.md) по реально реализованному поведению. Исследование оставить историческим источником предложения.
- [ ] Заполнить verification/handoff этого плана и только после закрытия обязательных gates отметить функцию complete.

## 6. Проверки и фиксация результатов

Команды ниже предназначены для реализации; сейчас приложение не меняется и они не считаются выполненными:

| Граница | Проверка |
| --- | --- |
| Pure core/input | `cargo test -p bazza-dictation -p bazza-text-input` после создания crates; OS-specific tests выполняются на своей ОС |
| ASR regression | `cargo test -p bazza-asr` с подготовленными native runtime libs; protocol tests отдельно от реального провайдера |
| Tauri orchestration | Targeted tests нового lifecycle/registry; `cargo check -p bazza` по setup из testing guide |
| TypeScript/контракты | `pnpm typecheck`; `pnpm exec vitest run src/lib/ipc.test.ts src/lib/mock.test.ts src/routes/Settings.test.tsx src/windows/DictationBar.test.tsx` после добавления новых tests |
| Финальный UI | `pnpm test`, `pnpm build` после успешных targeted checks |
| Документация | `node scripts/check-docs.mjs`, `git diff --check` |
| Native runtime | Synthetic harness, microphone/ASR end-to-end, app matrix и baseline записи; browser mock не заменяет эти проверки |

Автоматические тесты должны проверять поведение на границах: cancel/dispatch race, двойные команды, ownership clipboard, stale target/result, worker revocation и независимость настроек. Не заменять native matrix тестами, которые только повторяют внутреннюю реализацию.

Во время реализации создать один отчёт `docs/dev/test-reports/2026-11-02-system-dictation.md` (или с датой фактической приёмки). Хранить в нём версии сборок, fixture IDs и технические результаты, без аудио, текстов пользовательских диктовок, секретов и абсолютных машинных путей. Непроверенные ОС/сценарии явно оставлять открытыми.

## 7. Дополнения после базовой готовности

Выполняются отдельными задачами и не включаются автоматически в первый срез:

- Parakeet TDT v3 или другой локальный engine: отдельные adapter/artifacts/checksums/licenses и RU/EN bakeoff против текущего Whisper.
- Дополнительные cloud providers: собственные audio capabilities, consent, streaming/finalization и quota handling; текстовый LLM endpoint не считать STT API.
- Optional cleanup/словарь/app profiles: raw/cleaned раздельно, отдельный выбор локальной/облачной редактуры и отправки контекста.
- Сохранение истории/аудио и повторная расшифровка: явное включение, retention/deletion/export boundary.
- Параллельная встреча+диктовка: device sharing/fan-out, latency/resource scheduler, cloud registries; новый recording baseline до изменения capture.
- Непрерывная композиция текста в чужом поле: отдельная оценка TSF/IME, а не автоматическая вставка partials.

## 8. Verification и handoff текущей сессии

Реализация не начата, этапы 0–7 открыты. Подготовлен план на основе [исследования](../research/2026-11-02-system-dictation.md) и повторного чтения текущих границ кода. Native-билды, микрофонные проверки, clipboard injection и cloud transcription в рамках подготовки плана не запускались.

`node scripts/check-docs.mjs` и `git diff --check` прошли; новый plan-файл дополнительно проверен на whitespace через `git diff --no-index --check`. Независимо проверены актуальные точки интеграции и native gates. После вычитки уточнены non-abortable Whisper, порядок cleanup/reservation, повторная вставка во внешнюю цель, terminal profiles и отключение функции.

Следующее действие: **этап 0 — зафиксировать текущий meeting baseline и подготовить fixtures/harness; этап 1 — закрепить session/input contracts и общие lifecycle guards; этап 2 — проверить Windows insertion до подключения микрофона/ASR**. Кодовую реализацию начинать отдельным рабочим шагом; запрос этого чата ограничен подготовкой плана.
