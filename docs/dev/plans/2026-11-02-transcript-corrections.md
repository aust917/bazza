# Ручные исправления текста с сохранением оригинала

Статус: complete — код, адресные тесты, native migration/editor, selected-STT chain,
restart и перенос прошли на искусственных материалах Windows.
Обнаружен функциональный пробел при выполнении
[обновлённого roadmap](2026-10-30-local-fork-roadmap.md). Имена редактируются,
но native команды и UI правки текста реплики до этой задачи отсутствовали.
Это уточнение прежнего предположения о готовности программной части.

## Результат и границы

Пользователь исправляет отдельную реплику в обработанном представлении.
`segments.text` и сохранённые provider captions остаются оригиналами. Ручная
правка хранится в `clean_text` с признаком `clean_text_manual`; автоматическая
cleanup не перезаписывает её. Сброс правки — явное действие. Поиск и новая
генерация саммари используют исправленное представление; существующее саммари
не переписывается неявно. Источник и таймкоды не меняются.

Явный Re-transcribe заменяет сегменты и их обработанное представление. При
наличии ручных исправлений UI должен заранее объяснять эту замену; обычные
cleanup, rename, rediarize, restart и перенос сохраняют исправления.
Генерация агентом остаётся отдельным действием пользователя.

## Контракт и владение

- Store/native исполнитель: append-only migration v8 после завершения voice v7
  snapshot; `Segment.clean_text_manual` (serde default false), scoped atomic
  correction/reset API, cleanup protection, FTS/chunks/vector consistency,
  сохранение флага в `.bazza` и проверка некорректных сочетаний при импорте.
- Команда `set_transcript_correction`: `{id, segmentId, text}`; `text=null`
  сбрасывает исправление, строка сохраняет обработанный текст. Возвращает `Segment`.
  Разрешена для Ready-встречи, проверяет принадлежность сегмента и размер текста;
  тяжёлые индексы/embedding выполняются вне UI-потока, без неявного CLI.
- UI исполнитель: `Transcript.tsx`, адресный участок `Meeting.tsx`, `ipc.ts`,
  mock, связанные тесты и ru/en keys. Сохраняет новые Manrope/font-sans классы,
  Raw/Cleaned выбор, seek и доступность клавиатуры.
- Основной исполнитель: интеграция, native приёмка и продолжение selected STT chain.
  Capture/recovery код и результаты независимых задач не переписываются.

`.bazza` v1 получает добавочное поле сегмента с default false для отсутствующего
значения; raw/clean сохраняются раздельно. Это не возвращает поддержку файлов
прежнего бренда. Правила v1 и roundtrip должны быть явно проверены.

## Проверки и готовность

- [x] v7 → v8 сохраняет существующие сегменты, raw/clean, профили, samples и IDs.
  Native RUN2: все 26 прежних таблиц неизменны; профили/samples дополнительно
  покрыты migration unit fixture, поскольку эта библиотека была model-free.
- [x] Ручное исправление/сброс не меняет raw, provider provenance, время и спикера;
  другая встреча не может изменить чужой сегмент.
- [x] Cleanup сохраняет ручную правку, обрабатывая остальные сегменты; FTS и
  retrieval обновляются, старые vectors не остаются привязаны к изменённому тексту.
- [x] UI save/cancel/reset/error и ru/en работают; raw доступен, seek сохранён.
  Адресные UI-тесты и production native UI Save/Raw/Cleaned/blank/Reset/seek;
  Re-transcribe warning проверен отменой без вызова provider.
- [x] Restart и `.bazza` roundtrip сохраняют исправление и его ручной флаг.
- [x] Selected Soniox → local diarization → имена/исправление → cleanup → restart
  и импорт во вторую model-free библиотеку проходят настоящие native команды.

## Передача

Frozen voice v7 передан, migration v8 добавлена без изменения предыдущих миграций.
Store 48, share 21, desktop 34 passed / 1 ignored; Clippy затронутых пакетов passed.
TypeScript и 31 адресный UI/export/i18n тест passed. Ревью устранило гонки
позднего correction response, переименования спикера и reads во время Save:
UI сливает только поля правки и блокирует destructive Re-run до завершения.
Production UI (pnpm 10.34.9) и standalone EXE `c29013e9…` собраны.
Полный frontend набор 74 passed. Native RUN2
`v7-to-v8-native-verification.json` и
`native-ui-correction-2026-11-02T00-08-14-005Z/result.json` passed: оригинал,
provider metadata/media hashes сохранены, ручная пустая строка не заменяется raw,
Reset и seek работают, настройки/locale/route восстановлены.
Capture/recovery/per-app проверены отдельным стабильным v7 EXE; последующий
editor не объявляется покрытым тем snapshot. Реальная человеческая запись и
macOS остаются в сроках, сообщённых пользователем.

Итоговый EXE `5164ab49…` также пропускает пустые corrected SRT cues и нумерует
остальные подряд; шесть targeted export tests и production build прошли.
[Сквозной отчёт](../test-reports/2026-11-02-selected-stt-chain.md) подтверждает
настоящий complete Soniox → native gallery proposal/accept → правка имени/текста →
cleanup → local/Codex/Claude summary → restart → model-free B import/restart.
Raw, provider captions, manual flag и media hashes сохранены; explicit local
Re-transcribe отдельно подтвердил намеренную замену overlays и сохранность
предыдущего cloud contract. Проверки реальных людей и macOS этим не закрываются.
