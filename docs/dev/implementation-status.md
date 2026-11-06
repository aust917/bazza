# Сопоставление ТЗ с текущим Bazza

**Исправления следующего цикла, 2026-11-02:**
[capture reliability](test-reports/2026-11-02-capture-reliability.md)
подтверждает короткий native mic контроль с pause/Stop, исправленный per-app
capture с исключением чужого процесса и recovery A/B/C с сохранением предупреждений
после AI failures и restart. Одинаковое сравнение трёх видеорежимов прошло.
Полный 30-минутный mic/sys/A/V прошёл 39 technical checks; размах медиан
20,833/22,5 мс при лимите 40 мс. Поздняя проверка отсутствующего PID выявила
успешную активацию пустого Windows virtual client: итоговый startup guard прошёл
83 capture tests, Clippy и native absent/live PID повторы. Стенды закрыты,
пользовательские DB/WAL/SHM и указатели восстановлены.
[Soniox terminal fix](test-reports/2026-11-02-soniox-finalization.md) прошёл
26 unit tests, Clippy и 80-секундный native Stop со статусом complete.
Точная тишина обходит Whisper; model-backed tests Base/Small и шесть native
import/re-transcribe/quiet/control сценариев прошли. Обнаруженный пропуск
[редактора текста](plans/2026-11-02-transcript-corrections.md) закрыт кодом v8,
unit/UI тестами и production native UI;
[сквозной pipeline/перенос](test-reports/2026-11-02-selected-stt-chain.md) прошёл
с actual gallery proposal/accept, local/Codex/Claude summaries и model-free B.
[Voice matching](test-reports/2026-11-02-voice-matching.md) проверен отдельно
на fixed synthetic корпусе и v7 snapshot. Реальные EN/RU люди ожидаются 2026-11-03,
Mac — на неделе; общая продуктовая приёмка остаётся открытой.

**Предыдущая автоматическая приёмка, 2026-11-02:**
[сводный Windows-отчёт](test-reports/2026-11-02-full-automatic-acceptance.md)
добавляет 30 минут 1080p/30 с паузами, модели на искусственной речи, оба
подписочных агента, Voices, полный local pipeline и настоящие file choosers.
Общая приёмка остаётся отрицательной: микрофонные нулевые вставки, silent
process-loopback, Whisper на тишине, Soniox EOS и повреждённое crash-видео
имеют отдельные результаты. Прерванная обработка и отсутствие CLI отработали;
после restart проверены summaries/provenance, Voices и девять media players.
Ранняя отмена Claude сохраняет данные, но даёт неверную auth-диагностику;
пользователь выбрал не менять код. Физические устройства, реальные люди,
субъективное качество и macOS сохраняют отдельную область проверки.

Предыдущая пользовательская приёмка, 2026-11-02: после
[исправления дребезга](test-reports/2026-11-01-system-audio-fidelity.md)
пользователь записал 86,369 с. Прежних резких нулевых вставок нет, в том числе
на 23,42 с активного системного звука; контейнер/микширование проверены.
Пользователь не заметил дребезга и подтвердил совпадение A/V. Следующий прогон
56,72 с с pause/resume прошёл проверки файлов; пользователь подтвердил A/V
после Resume. Stop на паузе проверен прогоном 17,66 с; после перезапуска
карточка/seek доступны. Прогон 6:26 с idle после 30 с прошёл проверки;
пользователь подтвердил A/V и отсутствие выключения экрана. Затем
[проверены смена Windows default output и A/V после возврата](test-reports/2026-11-02-capture-device-acceptance.md):
шесть восстановлений, затем отдельный контроль с 43 парами сигнал/вспышка.
Медианы audio−video: +76,693 / +1,693 / +73,359 мс; возврат отличается
от начала на −3,334 мс при видеоточности около 33 мс. Базовое запаздывание
около 75 мс на Voicemeeter сохраняется; его причина тестом не разделена.
[Технические автопрогоны 720p и SRT/VTT](test-reports/2026-11-02-automated-product-acceptance.md)
пройдены: 1280×720/nominal 30 FPS, 22 пары маркеров, native импорт через IPC,
точные данные и playback/seek после перезапуска. Субъективное качество
720p и физическая потеря устройства остались открытыми;
получасовой стресс, модели/сервисы и file choosers проверены позднее в сводном
отчёте выше. Там же измерены CPU/RAM 720p и 1080p в разных условиях.
macOS и чужие display requests в idle-прогоне не измерялись.

[Автоматический `.bazza` roundtrip между двумя Windows-библиотеками](test-reports/2026-11-02-bundle-runtime-acceptance.md)
проверил точные raw/clean/summary/tags/highlights/provenance и media hashes,
CRC partial failure, transaction rollback при конфликте каталога, retry/duplicate
и настоящий playback/seek после restart. Это искусственные данные, без генерации AI.

Первичный аудит: 2026-10-30; текущее состояние обновлено 2026-11-02.
База исходников первичного аудита: `917e2a0`; в рабочем дереве также
есть локальные изменения правил разработки. Это анализ кода, а не протокол
успешной записи. На момент первичного аудита аппаратная запись, модели и внешние
сервисы не запускались; последующие испытания перечислены отдельно ниже.

После аудита подготовлены desktop-сборка и [отчёт baseline](test-reports/2026-10-30-baseline.md):
21 frontend-тест и 97 Rust-тестов прошли. Затем выполнена
[реальная контрольная запись](test-reports/2026-10-31-recording-control.md):
экран и аудиодорожки сохраняются, воспроизведение/seek работают; измерен рост
отставания итогового микса без паузы и опережение звука после паузы, повторённое
дважды. Выводы ограничены испытанными Windows-устройствами и настройками.
На этапе baseline код не изменялся; качество речи и модели не проверялись. Последующая
[диагностика](test-reports/2026-10-31-sync-diagnosis.md) установила накопление
времени при поблочном смешивании и паузу только аудио; начальный offset и
внутреннее поведение FFmpeg при обрезке требуют отдельного рассмотрения.

Затем выполнено [локальное исправление](test-reports/2026-10-31-sync-fix.md):
FIFO микса, устойчивые интервалы видеопаузы и сохранение полного аудио при mux.
Прежнее накопление и скачок не воспроизвелись в контрольных прогонах; границы
аппаратной/offline-проверки финального EXE и остаточный offset явно описаны в отчёте.

Следующая итерация [проверена 2026-11-01](test-reports/2026-11-01-capture-acceptance.md):
общие capture clocks, защита дисплея от idle-off и диагностика FFmpeg. Ложный fallback
из-за stale WASAPI callback timestamp исправлен; новые прогоны используют native
capture timestamps. Без пауз, с тремя паузами, stop на паузе и 370 с idle не выявили
прежнего накопления/скачка; длинная запись без чёрных кадров в секундной выборке.
Остаточная задержка тестовой цепочки и точные ограничения описаны в отчёте.
Отказы writer/video сохраняют доступные файлы и сообщают об ошибке; UI качества,
другие устройства, macOS и реальные речь/модели требуют отдельной приёмки.

Требования и их идентификаторы определены в [ТЗ](../product/requirements.md).
Последовательность работ — в [плане](plans/2026-10-30-local-fork-roadmap.md).
Этот документ хранит факты реализации и ограничения, не дублирует требования.
Полная матрица записи описана в [тест-плане baseline](test-plans/2026-10-30-recording-baseline.md).
Непроверенные наблюдения по capture-коду остаются гипотезами. После baseline
пользователь отдельно разрешил исправления подтверждённых проблем; это не означает
необходимость менять работающий захват ради будущих настроек качества.

## Краткая матрица

После capture-исправлений выполнена [программная работа без ручной приёмки](plans/2026-11-01-independent-product-work.md).
Текущая матрица учитывает её; подробности первичного аудита ниже сохранены как
история обнаруженных пробелов. Новые источники фактов:
[импорт](../import.md), [голосовые образцы](voice-memory.md),
[агенты саммари](summary-agents.md) и
[итоговые проверки](test-reports/2026-11-01-independent-product-work.md).

«Есть» означает наличие пути реализации в исходниках, а не подтверждённую
работоспособность на конкретном компьютере. «Частично» — основа есть, но требуемый
сценарий, настройка или интеграция ещё не покрыты целиком.

| ID | Область | Статус по коду | Основной остаток |
| --- | --- | --- | --- |
| R1 | Микрофон и системный звук | Native fixes, per-app и полный 30-минутный mic A/V прошли | Native 44,1 kHz, default-input follow, физическая потеря, ongoing parent death/descendants и несколько выходов не приняты |
| R2 | Видео выбранного экрана и качество | 720p/1080p, одинаковое сравнение и длинная регрессия прошли | Три режима × 120 с и 30 минут: decode/PTS/A/V/CPU/private commit, restart/seek проверены; субъективное качество и macOS отдельно |
| R3 | Локальный и облачный STT | Exact-zero bypass, Soniox Stop и selected-STT chain проверены native | Три zero import/re-transcribe, EN/RU holdouts, тихая речь и Soniox80с complete; chain/restart/import passed, настоящий reconnect не испытан |
| R4 | Диаризация | Model-backed synthetic проверки пройдены | Pyannote/TitaNet: 1–3 голоса, finite embeddings, holdout/unknown; реальные люди, DER и восемь участников не приняты |
| R5 | Единая база голосов | Native synthetic lifecycle/reopen и безопасные предложения проверены | Fixed корпус: 52 cases, 0 ложных имён для 24 unknown segments; 75/160 known segments без предложения. Real quality и native v8 selected-STT chain отдельно |
| R6 | Локальная библиотека | Recovery, native редактор v8 и перенос проверены | Повреждённое видео скрыто, аудио/warning сохраняются через AI failure/restart; raw неизменен при правке текста, overlays переживают cleanup/restart/import. Power loss отдельно |
| R7 | Обмен и импорт с транскриптом | SRT/VTT, `.bazza`, manual overlay и file choosers проверены | Selected cloud + исправления перенесены в model-free B, playback/20 cue seeks/restart passed; другие платформы/кодеки, большие пакеты и сбой во время импорта отдельно |
| R8 | Саммари через Claude Code / Codex | Оба подписочных runner проверены на artificial input | Short/long, reopen/cancel/oversize, missing CLI, сохранность/provenance проверены; ранняя отмена Claude даёт неверную диагностику, общее качество отдельно |
| R9 | Локальная разработка и явные передачи | Явные интеграции проверены | Локальные defaults, Soniox consent/key revoke и Generate проверены; внешние тесты используют только искусственные данные, публикации не было |

## Подробности первичного аудита и последующих capture-исправлений

Следующие разделы описывают состояние до продуктовой работы 2026-11-01.
Указания «отсутствует/нет» в них являются исходными наблюдениями, а не текущим
статусом. Текущие результаты и ограничения приведены в матрице и связанных выше
документах.

### R1–R2: захват аудио и видео

- [Recorder](../../crates/bazza-capture/src/recorder.rs): `RecordConfig`,
  `Recorder::start`, `open_sys`, `run_worker` пишут `mic.wav`, `sys.wav`, `mix.wav`;
  архивное аудио — 48 кГц, поток для распознавания — 16 кГц mono.
- `open_sys` записывает весь звук с одного выбранного аудиовыхода или с выхода
  по умолчанию. При неудаче остаётся запись микрофона. Захват нескольких
  аудиовыходов требует отдельной реализации.
- [Settings UI](../../app/src/routes/Settings.tsx) позволяет выбрать микрофон,
  output-устройство и `capture_system`.
- [Popup](../../app/src/windows/Popup.tsx) показывает аудиовыход и микрофон.
  Системный звук можно отключить в настройках.
- [Видео](../../crates/bazza-capture/src/video.rs): `VideoTarget`,
  `ffmpeg_capture_args`, `VideoRecorder::start` поддерживают выбор монитора;
  [список источников](../../src-tauri/src/commands/devices.rs) и Popup дают выбор
  экрана/окна. Захват отдельного окна реализован для Windows; macOS для такого
  target использует экран, что нельзя представлять как полное равенство платформ.
- [Settings](../../src-tauri/src/settings.rs) хранит `video_fps=15` и
  `video_max_height=1080`. `common_encode_args` фиксирует `libx264`, `veryfast`,
  `CRF=28`; UI выбора профиля качества/FPS/разрешения сейчас отсутствует.
- [Оркестрация записи](../../src-tauri/src/recorder.rs): `start` запускает аудио и
  видео; `stop` завершает оба; `recover_orphans` подбирает прерванные встречи.
  После baseline `pause`/`resume` согласуют аудиосостояние с устойчивыми интервалами
  `VideoRecorder`; фактический видеозахват остаётся непрерывным, паузы вырезаются
  при обработке. Production pipeline использует `mux_recording`: видео без пауз
  копируется, с паузами перекодируется; предел по длительности WAV не обрезает
  звук при более коротком видео. Ошибка оставляет raw/metadata для повторной
  обработки, без публикации непрерывного raw. Измерения и ограничения — в отчёте
  исправления; UI качества не требует новой замены capture engine или defaults.

## R3–R4: транскрипция и диаризация

- [WhisperEngine](../../crates/bazza-asr/src/whisper.rs) выполняет локальные
  `transcribe_with`/`transcribe_opts`; `Caption` содержит временные метки и
  `is_final`. [LiveTranscriber](../../crates/bazza-asr/src/live.rs) выдаёт уточняемую
  строку примерно каждые 2 секунды и фиксирует её после паузы или длинного окна.
- [recorder.rs](../../src-tauri/src/recorder.rs) передаёт смешанный PCM в
  `LiveTranscriber`; [LiveFeed](../../src-tauri/src/live_feed.rs) держит live-текст
  в памяти; [LiveView](../../app/src/components/LiveView.tsx) и record bar его
  отображают. В live-`Caption` нет спикера: live-диаризация не реализована.
- [Pipeline](../../src-tauri/src/pipeline.rs), `post_process_with`, после остановки
  заново создаёт итоговый транскрипт через Whisper. Soniox-потока недостаточно
  добавить только в UI: нужно определить, чей final-текст сохраняется, чтобы
  последующий локальный проход не перезаписывал выбранный облачный результат.
- Soniox, параметры облачного STT и общий STT-provider интерфейс в действующих
  исходниках не обнаружены. `LiveTranscriber` зависит непосредственно от
  `WhisperEngine`. Существующий OpenAI-compatible endpoint обслуживает LLM,
  а не распознавание речи.
- [DiarizeEngine](../../crates/bazza-diarize-cli/src/engine.rs) использует sherpa-onnx
  с локальными pyannote segmentation 3.0 и NeMo TitaNet small. Модели выбирает
  `AppState::diarize_models` в [state.rs](../../src-tauri/src/state.rs).
  **Pyannote уже используется локально; облачного Pyannote API здесь нет.**
- `diarize_raw`, `map_diarization`, `rediarize` в pipeline запускают sidecar,
  назначают спикеров сегментам и позволяют повторить диаризацию отдельно.
  [consolidate.rs](../../crates/bazza-diarize/src/consolidate.rs) ограничивает число
  спикеров константой `MAX_SPEAKERS=8`; это важно для приёмки больших созвонов.

## R5: единая база голосов

- [Схема БД](../../crates/bazza-store/src/migrations.rs), v2, уже содержит
  `speakers` и `meeting_speakers`. В профиле — имя и один embedding, не коллекция
  исходных образцов. Это текущая основа одной базы голосов, а не другая функция.
  [Store::rename_speaker](../../crates/bazza-store/src/speakers.rs)
  создаёт профиль, обновляет усреднённый embedding, объединяет одинаковые имена.
- `map_diarization` сравнивает новые векторы с известными через
  [best_match](../../crates/bazza-diarize/src/identify.rs); порог предложения
  `SUGGEST_THRESHOLD=0.62` фиксирован. `find_me_cluster` отдельно оценивает, какой
  кластер принадлежит локальному микрофону; это не распознавание личности.
- [Команды спикеров](../../src-tauri/src/commands/speakers.rs) реализуют
  `rename_speaker`, `accept_speaker_suggestion`, список и удаление известных лиц.
  [SpeakersPanel](../../app/src/components/SpeakersPanel.tsx) и
  [SpeakerChip](../../app/src/components/SpeakerChip.tsx) дают исправление, merge и
  подтверждение «Is this X?». Перестраиваются retrieval и существующее саммари.
- Нет отдельного экрана базы голосов, импорта эталонного аудио, нескольких
  управляемых образцов на человека и происхождения/версии модели embedding.
  Удаление известного голоса есть в IPC, но пользовательский экран не найден.
  При добавлении backend нельзя сравнивать несовместимые embeddings как одну базу.

## R6–R7: библиотека и перенос записей

- [Library](../../app/src/routes/Library.tsx), [Meeting](../../app/src/routes/Meeting.tsx)
  и [Player](../../app/src/components/Player.tsx) уже обеспечивают список встреч,
  аудио/видео и `seek` по времени из транскрипта. SQLite, файлы и каталог данных
  принадлежат [bazza-store](../../crates/bazza-store/src/db.rs).
- [export_bundle / import_bundle](../../crates/bazza-share/src/bundle.rs) переносят
  `.bazza`: медиа, raw/clean-текст, имена спикеров, саммари, tags и highlights.
  Повторный meeting ID пропускается, а не обновляет существующую встречу.
  Глобальные `speaker_id`/`suggested_id` сбрасываются **при импорте**;
  embeddings и исходная база голосов в пакет не включаются.
- [Команды обмена](../../src-tauri/src/commands/organize.rs) реализуют экспорт,
  импорт, HTML и включаемые LAN-ссылки; UI — [ShareDialog](../../app/src/components/ShareDialog.tsx).
  Это доступные механизмы обмена, а не облачный хостинг записей.
- [Импорт медиа](../../src-tauri/src/import.rs), `import`, конвертирует файл
  через ffmpeg и запускает обычную обработку. Совместного импорта медиа с готовым
  SRT/VTT-транскриптом нет; существующий SRT-export не решает эту задачу.
- Чужой `.bazza` пока нельзя считать надёжной границей ввода: версия manifest
  читается без проверки поддерживаемости, `meeting.id` и имена media напрямую
  участвуют в путях без явного ограничения каталогом/списком допустимых имён.
  Нужны проверки traversal, размеров и содержимого до записи файлов.
- Импорт пишет файлы и несколько сущностей БД без общей атомарности; сбой может
  оставить частичный результат. Отсутствующая ZIP-media пропускается, а
  `has_video` выводится из списка manifest, не фактического наличия файла.
  Требуются отрицательные тесты, rollback и понятный отчёт для пользователя.

## R8–R9: саммари, подписки и локальный режим

- [TemplatesEditor](../../app/src/components/TemplatesEditor.tsx),
  [команды AI](../../src-tauri/src/commands/ai.rs) и
  [хранилище AI](../../crates/bazza-store/src/ai.rs) уже дают шаблоны и повторную
  генерацию. [run_summary](../../src-tauri/src/summary_stage.rs) подставляет
  транскрипт/участников, вызывает генерацию и сохраняет результат у встречи.
- [LlmBackend](../../crates/bazza-llm/src/backend.rs) и `AppState::llm` поддерживают
  bundled llama.cpp и OpenAI-compatible endpoint. Тот же выбор используется для
  cleanup, chat, running notes и заголовков, а не только итогового саммари.
- Вызов Claude Code/Codex CLI, выбор агента для саммари, управление его процессом,
  авторизацией и результатом отсутствуют. Подписочная авторизация и пригодность
  неинтерактивного вызова не проверялись; нельзя подменять это обычным API-ключом
  или считать подписку гарантией программного доступа.
- Нужна граница именно для summary runner, чтобы выбор внешнего агента не
  отправлял ему автоматически live notes, chat и cleanup. Локальный запуск CLI
  сам по себе не означает локальную обработку переданного транскрипта.
- `AGENTS.md` закрепляет локальную разработку; в локальном Git нет `origin`,
  `upstream.pushurl=DISABLED`, `push.default=nothing`. Другие копии проекта вне
  этой папки не проверялись, их отсутствие или удаление здесь не утверждается.
  После [переименования в Bazza](plans/2026-11-02-bazza-rename.md) updater удалён,
  а `auto_update_check` по умолчанию выключен; существующие сети описаны в
  [privacy](../privacy.md).

## Наличие тестов и пределы доказательств

Первичный аудит проверял исходники тестов. После него локально прошли frontend
typecheck, 7 файлов/21 UI-тест, frontend build, нативная сборка и 97 Rust-тестов;
короткие аппаратные прогоны и их ограничения описаны в отчёте 2026-10-31.
Эти виды доказательств нельзя подменять друг другом:

- `bazza-capture`: DSP mixing/resampling и построение ffmpeg capture/mux arguments;
  это не доказательство синхронизации реальной многоминутной записи.
- `bazza-asr`: логика partial/final, silence и hallucination filtering; ignored
  `transcribes_jfk_fixture` требует локальную Whisper-модель.
- `bazza-diarize`: threshold/centroid, cluster assignment, sharding/consolidation;
  `bazza-store::speakers`: rename/merge/suggestions и перестроение chunks.
- UI: `SpeakerChip`, `SpeakersPanel`, `Transcript` проверяют подписи и seek.
- `bazza-share::bundle_roundtrip_merges_without_duplicates` проверяет базовый
  roundtrip; server integration — LAN-page/range media. Это не полный набор
  отрицательных тестов чужого архива или поддержка внешних форматов транскрипта.
- Шаблоны/summary storage и pure helpers имеют тесты; Soniox, облачный Pyannote
  и подписочные CLI-адаптеры отсутствуют, проверок их интеграции пока нет.

Следующий источник действий — [поэтапный план](plans/2026-10-30-local-fork-roadmap.md).
Успешная runtime-приёмка должна отдельно зафиксировать ОС, устройства, выбранные
модели/провайдеры и реальные результаты, без публикации пользовательских записей.
