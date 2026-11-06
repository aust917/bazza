# Bazza: микрофон и восстановление после сбоя

Дата: 2026-11-02, Asia/Yekaterinburg. Статус: доступная автоматическая Windows-часть
прошла: mic/recovery/per-app, Soniox/Whisper, редактор v8 и selected-STT chain,
сравнение видео и полный 30-минутный mic A/V. Стенды закрыты, исходные указатели и
DB/WAL/SHM восстановлены. Аппаратные, человеческие и macOS-проверки открыты.
[План задачи](../plans/2026-11-02-capture-reliability.md) ·
[Общий roadmap](../plans/2026-10-30-local-fork-roadmap.md).

## Исходники и координация

Работа идёт в общем локальном дереве с переименованием Bazza, русской
локализацией и исправлением voice matching. Владение файлами и Cargo/runtime
согласовано между задачами. Пользовательские встречи внешним сервисам не отправлялись.

- Windows microphone использует capture endpoint с native frame positions,
  без LOOPBACK, с прежним default buffer duration. Выбранный input сохраняется;
  default-follow включается только при нефиксированном input. macOS CPAL сохранён.
- Recovery строго декодирует RAW и временный MP4 до публикации; сохраняет inputs
  и предыдущий результат при отказе. Короткое валидное видео с более длинным
  аудио допускается; отсутствующие до аварии кадры не восстанавливаются.
- Дополнительные source-исправления после первого EXE: `get_meeting` скрывает
  retained invalid MP4 при `has_video=false`; отдельный типизированный sidecar
  сохраняет video warning при cleanup/summary/rediarize/deferred AI и restart.
  Sidecar исключён из media allowlist экспорта и удаляется с записью.
- Per-app source-исправление по trace описано ниже. Пакетная диагностика включается
  только `BAZZA_CAPTURE_PACKET_TRACE`, ограничена 64 строками на stream и не пишет PCM.
- ASR обходит inference для пустого/точно нулевого PCM; тихая ненулевая речь
  проходит обычную обработку. Soniox guard не ослаблялся; добавлена числовая диагностика.

## Проверки до первого native snapshot

Подготовленный локальный release CPU toolchain, Windows:

| Проверка | Фактический результат |
| --- | --- |
| `cargo test --release -p bazza-capture --locked --offline` | 78 passed, 5 ignored |
| `BAZZA_TEST_FFMPEG`, `ffmpeg_ -- --ignored` | 4 passed: повреждение, audio-only, delayed/short video, паузы |
| `cargo test --release -p bazza-asr --lib --locked --offline` | 21 passed |
| Whisper Base и Small, `transcribe_fixture -- --ignored` | По 3 passed: JFK, точная тишина с prompt/offset, тихая речь между паузами |
| `cargo clippy --release -p bazza-capture -p bazza-asr --all-targets --locked --offline -- -D warnings` | passed |
| `cargo test -p bazza --release --lib --locked --offline` | 27 passed, 1 ignored |
| `cargo build -p bazza --release --features tauri/custom-protocol --locked --offline` | passed, UI встроен без Vite server |

Логи — `.git/local-tools/reliability-*.log`. Эти результаты предшествуют новым
per-app/diagnostic/API-правкам и следующему общему snapshot; они не подтверждают
более поздние исходники. Дополнительные combined checks ведутся отдельно.

После per-app/API/diagnostic/voice и strict-format изменений общий исполнитель
проверок получил 79 passed / 5 ignored для capture, 33 passed / 1 ignored для
desktop lib (включая три diagnostic regressions) и 81 passed для store/share/diarize.
Последний набор включает запрет экспорта retained invalid video. Логи:
`.git/local-tools/bazza-strict-rename-{capture-final,desktop-isolated,crate}-tests.log`.
Для lib tests использовался process-local `TAURI_CONFIG`, отключающий только
копирование bundle resources/sidecars; конфигурация и frozen EXE не менялись.
Это программная проверка новых исходников перед следующим native snapshot.

Последующее hardening добавило suppression только стартового process
discontinuity и regression estimated frame order через pause/fresh-clock reopen:
capture **81 passed / 5 ignored**, лог
`.git/local-tools/reliability-capture-hardened-tests.log`. Общая stall policy,
endpoint/microphone discontinuity и platform guards этим не менялись.

## Первый native snapshot

EXE 28 334 080 B, SHA-256
`93814ba996a400fe15c02d7a45e580fa743a4bdbb8b773c6cea1a90426b5ab0b`.
Библиотека и evidence:
`.git/local-tests/20261102-capture-reliability-20261101T225906-962079Z/`
(далее **RUN**). Loopback CDP 9225, model-free библиотека, фиксированные виртуальные
CABLE input и VM3 output; Windows default devices не менялись. Main и recordbar
перемещены на DISPLAY2. Только искусственные звуковые стимулы.

### Микрофон: независимый callback-order контроль

Контроль сохраняет PCM напрямую через CPAL без placement по timestamps.
На вход CABLE поданы два различных шестисекундных сигнала. Анализатор требует
source/direct correlation ≥0,98, проверяет резкие нулевые вставки отдельно,
точную тишину sys и совпадение mix с суммой источников (допуск 1 PCM unit).

| Попытка внутри RUN | Результат |
| --- | --- |
| `mic-fidelity-2026-11-01T23-04-07-347Z` | Все waveform checks прошли, mic/reference около 0,99484; общий harness failed из-за единственного нетаймированного warning независимого capture |
| `mic-fidelity-2026-11-01T23-09-45-201Z` | Failed: резкая вставка 499 samples одновременно в mic и независимом raw, с совпадающими краями; это не доказательство вставки только приложением |
| `mic-fidelity-2026-11-01T23-11-21-184Z` | Passed: pause/resume 3 с и Stop на паузе, все waveform/source/direct/mix checks, настройки восстановлены |

После первой попытки независимый диагностический helper дополнен временем и
числом принятых frames для каждого warning; прежний helper/результат не переписаны.
Во второй попытке warning был типизированным Xrun до первого принятого frame
на 10,583 мс, задолго до стимула. Поздние/неизвестные warnings остаются блокирующими.
Пороги waveform-анализа не менялись. Совпадающая вставка во втором raw/app
сохраняется как ограничение исходного виртуального маршрута, без приписывания
причины Windows/драйверу/проигрывателю без дальнейшей диагностики.

Это ограниченная проверка virtual input 48 kHz. Native input 44,1 kHz,
переключение default microphone, физическое отключение и длительный mic A/V
этими короткими попытками не закрыты.

### Startup recovery: повреждённый RAW и surviving MP4

`RUN/recovery-seed-7195834fb32c4b14942488fb1f1481a3/native-verification.json`:

- A, исходный повреждённый RAW: Ready с video error, `has_video=false`, RAW/WAV
  сохранены по hash, audio playback/seek прошёл.
- B, повреждённый surviving MP4 без RAW: корректные video error/`has_video=false`,
  WAV/MP4 сохранены, но первый EXE всё ещё возвращал файл в `media.video`;
  native player check failed. API guard исправлен в последующих исходниках.
- C, валидное короткое видео с более длинным аудио: Ready без video error,
  полный decode и native playback/seek прошли.

Общий outcome этой приёмки **false** до повтора B на обновлённом EXE. Аварийные
файлы скопированы из прежней искусственной попытки; служебный clock marker
нормализован только в копии. Оригинальные evidence/hash не изменены.

### Per-app: установленная причина и source-исправление

`RUN/process-loopback-probe-2026-11-01T23-08-24-915Z/` — failed:
при полном проигрывании owned 10,125 с речи sys содержал лишь 1197 нулевых
samples (24,9375 мс), mic/mix — 14,168979 с нулей.

Все 64 наблюдаемых process packets имели 480 frames, flags=0,
`device_position=0`; QPC рос на 100000 единиц (10 мс). Первый packet дал 480 frames,
остальные — ноль: resampler принимал нулевую native position за дубликаты.
Это прямое наблюдение механизма потери PCM, а не прежняя гипотеза.

Исправление в исходниках использует delivered frame order для виртуального
process client; endpoint/microphone продолжают использовать native positions.
Такая process-временная привязка честно помечается estimated. Добавлен regression
с 64 zero-position packets и QPC jitter; native повтор на новом EXE обязателен.

### Soniox: короткий Stop прошёл, прежний длинный дефект открыт

`RUN/soniox-stop-probe-2026-11-01T23-05-58-491Z/` — passed:
до чтения ключа выполнены local-only тишина и source correlation 0,98967;
20-секундный synthetic cloud capture завершил transcript как complete, три
подтверждённых фрагмента. Настройки восстановлены, private key удалён.
Invalid watermark не воспроизведён, его числового failure trace в этой попытке нет.
Это не закрывает прежний отрицательный long EOS outcome и настоящий reconnect.

## Завершение первого runtime и следующий шаг

RUN session завершён 2026-11-02 04:12 местного времени: 10 тестовых встреч,
нет Recording/Processing, local provider/consent=false, private key отсутствует,
приложение штатно остановлено, port 9225 свободен. Указатели восстановлены
побайтно; исходные DB/WAL/SHM неизменны. `before-stop-checks.json` и session
verification сохраняют эти проверки. Runtime передан голосовой задаче.

Далее: combined crate/desktop tests новых source-исправлений, новая standalone
сборка, повтор B и сохранности диагностики после AI-действий, per-app source
и exclusion чужого процесса, затем оставшиеся STT/видео-регрессии roadmap.
Проверки реальных людей ожидают контрольную запись пользователя 2026-11-03;
Mac пока недоступен и ожидается на неделе. Удаление старой файловой совместимости
согласовано с новой задачей переименования; прежняя legacy acceptance отменена.

## Объединённый snapshot и повтор recovery/per-app

Private EXE 28 531 712 B, SHA-256
`839f12e7bcd2defbfb3d2e367565e2edef9aa2435c431e194d2a659dd810d97b`.
В нём общие capture/recovery fixes, voice v7 и новые Manrope-шрифты;
последующая migration v8 и редактор текста ещё не включены. Все 14 runtime
файлов скопированы с проверкой hash. Новая model-free библиотека:
`.git/local-tests/20261102-capture-reliability-20261101T231533-946850Z/`
(далее **RUN2**). Пользовательская библиотека не открывалась.

`RUN2/recovery-seed-934d89d37e654b3da8fd32d60e9a403a/manifest.json`
и native verification: **A/B/C passed**. Повреждённые RAW и surviving MP4
сохранились по hash, `has_video=false`, `media.video=null`, warning доступен;
целое короткое видео с полным аудио прошло strict decode и native playback/seek.
Провал B первого snapshot закрыт этим отдельным повтором.

`RUN2/native-reliability-followup-2026-11-01T23-48-28-236Z/`: **passed**:

- A/B video warning и структурированная диагностика пережили cleanup с
  отсутствующей LLM, ожидаемый summary preflight rejection и отказ rediarize
  без установленной модели. Ошибки отдельных AI-этапов не подменили video warning.
- Выбранная речь PID присутствует в sys; посторонний процесс на том же VM3
  исключён, включая отдельное окно длительностью более секунды без target-речи.
  Mic — точный ноль, mix согласован с источниками; packet trace ограничен.
- Настройки восстановлены, listeners удалены, recorder Idle, моделей и
  сохранённого Soniox key нет.

Предыдущая followup-попытка `23-46-54-256Z` остановилась до capture: corpus WAV
не имел ожидаемого трёхсекундного нулевого префикса. Helper создаёт отдельный
derived WAV с точным префиксом и проверяет hash исходника; это изменение стимула,
а не допуск к неверному результату приложения.

После штатного завершения и запуска **того же EXE**
`RUN2/native-reliability-reopen-2026-11-01T23-54-43-980Z/` — **passed**:
оба warning/sidecar сохранены, повреждённое видео скрыто, аудио и per-app
медиа доступны после reopen. Настройки восстановлены, recorder Idle.

## Длинный Soniox: фактическая граница EOS

`RUN2/soniox-stop-probe-2026-11-01T23-49-52-304Z/` — **failed**:
80,038 с synthetic capture, 20 подтверждённых фрагментов, source correlation
0,99024048. Текст и локальные медиа сохранены, transcript имеет статус incomplete.
Настройки восстановлены, private key удалён.

Числовой trace финального ответа: offset=0, sent=1 280 615 samples,
acknowledged=1 280 640, `final_audio_proc_ms=80040`, `finished=true`,
EOS sent/input closed=true. Превышение — **25 samples (1,5625 мс)**,
buffer range 1 207 680–1 280 615. Таким образом, ошибка воспроизведена с точными
значениями. Само наблюдение не доказывает причину provider padding/rounding.
Исправление требует проверки terminal semantics и локального transport regression;
безусловное ограничение watermark до sent не принимается.

## Дополнительный функциональный пробел

При подготовке selected-STT chain обнаружено отсутствие текстового редактора
реплики. В исходниках добавлены migration v8, scoped correction API, защищённое
manual cleaned representation и UI; отдельные Store/share/desktop/UI тесты прошли.
Этот более поздний функционал не объявляется принятым snapshot выше. Интеграция,
native restart/перенос и границы повторной обработки ведутся в
[плане исправлений текста](../plans/2026-11-02-transcript-corrections.md).

## Контролируемое сравнение видео

`RUN2/video-settings-compare-95a56e64-4e4e-4e84-ad5b-8a5ee62e4226/`:
**все три режима passed**, по 120 с одинакового 1920×1080/30 synthetic стимула,
тот же DISPLAY2 и точный VM3 endpoint, mic — ноль. `acceptance.json` связывает
capture result и отдельный `analysis.json` по SHA-256; captures запускались
с отложенным анализом. Все четыре media файла каждого режима строго декодируются;
формат, cadence/PTS, first/middle/last markers и исходные hashes прошли.
Настройки восстановлены; compiler/inference/постороннего FFmpeg во время
измерения не наблюдалось. Analysis и новая сборка начаты после всех captures.

| Режим | Средний CPU всего Bazza tree¹ | Пиковый private commit | MP4 | Размах трёх AAC−video медиан |
| --- | ---: | ---: | ---: | ---: |
| 1080p / 15 FPS | 2,348% | 1363,15 MiB | 30 285 356 B | 1,666 мс |
| 720p / 30 FPS | 2,897% | 721,96 MiB | 11 382 516 B | 2,501 мс |
| 1080p / 30 FPS | 3,805% | 1373,77 MiB | 33 263 476 B | 0 мс |

¹ Взвешено по фактическим интервалам, нормировано на 28 логических CPU;
включает WebView/encoder, исключает stimulus players. По 60 capture samples,
покрытие интервалов более 98%. Working set с повторно посчитанными shared pages
не выдаётся за уникальное потребление памяти. Порядок фиксирован, прогоны
однократные; cache/thermal/неизвестный фон остаются ограничениями.

Абсолютные AAC−video медианы: около −197 / −142 / −119 мс соответственно.
Раздельный запуск visual и exact-endpoint PCM players входит в эту разность;
это не доказательство такого же постоянного offset у реального созвона и не
основание компенсации в capture. Точность зависит от FPS. Субъективное качество,
отсутствие отдельных пропусков кадров и универсальный performance threshold
этим сравнением не принимаются.

Попытка `video-settings-compare-66f2ef85-2967-4dc3-90b6-5e4f8754dff6`
остановилась **до capture**: gate ошибочно считал открытый Claude Desktop и
собственный runner с fixture-путём задачами inference. Адресный classifier
исправлен: CLI требует noninteractive mode, скрипт — точный entrypoint basename;
12 изолированных classifier cases прошли. Cargo/native inference/tests и внешний
FFmpeg продолжают блокировать измерение. Процессы пользователя не завершались.

Новая production UI успешно собрана pnpm 10.34.9; полный frontend набор —
74 passed. ASR terminal source — 26 unit passed и Clippy passed;
см. [отдельный отчёт](2026-11-02-soniox-finalization.md).

## Snapshot v8: миграция, UI и исправленный Soniox

Standalone EXE 28 564 992 B, SHA-256
`c29013e9f5e87f5cc0bfe82fb1fcb7eff9290723199ff8ecb855a603ce4d4885`:
production frontend, migration v8, ручная правка текста и terminal Soniox fix.
Перед запуском сохранена SQLite backup тестовой v7-библиотеки. После native
startup `v7-to-v8-native-verification.json` — **passed**: все 26 прежних таблиц
сравнены по старым столбцам/rows/hash без изменений, новое поле везде false,
integrity check ok. В model-free RUN2 не было voice profiles; их сохранность
покрывает отдельный Store migration fixture, а не эта пустая таблица.

`RUN2/native-ui-correction-2026-11-02T00-08-14-005Z/` — **passed**:
реальный импорт собственного 12-секундного WAV+SRT; production DOM Save,
Raw/Cleaned, пустая ручная правка с placeholder, Reset и seek к 2 с. Оригинал,
provider metadata и hashes медиа неизменны; Re-transcribe warning отменён без
вызова provider и замены сегментов. Настройки, locale, маршрут, listeners и
confirm spy восстановлены; recorder Idle, моделей/ключа нет.

`RUN2/soniox-stop-probe-2026-11-02T00-08-19-093Z/` — **passed**:
80 088 мс, 20 подтверждённых captions, Soniox complete; source correlation
0,99024045. Локальные gates прошли до чтения ключа; настройки восстановлены,
private value удалён. Терминальная статистика больше не используется как replay
ACK после подтверждённого EOS; nonterminal guard и caption bounds сохранены.
Прежние точные 25 samples воспроизводятся unit/transport regression; повторное
появление именно этой числовой границы у сервиса не заявляется.
Настоящий reconnect остаётся отдельной непроведённой native проверкой.

## Первый длинный mic A/V-повтор: остановка из-за нагрузки

`RUN2/long-capture-regression-9c2fbf49-9bcc-4d54-8031-f063a06bf3fc/`
не является успешным 30-минутным тестом. Offline detector calibration прошла;
на capture около 222 с строгий workload gate обнаружил новый `claude.exe` print
процесс PID 52944, созданный примерно за восемь секунд до sample. Он завершился
сам до read-only проверки; command/prompt не сохранялись и процесс не закрывался.
Root не запускал этот CLI в рамках текущего теста.

Owned capture штатно остановлен, частичная встреча Ready (221 739 мс, видео
доступно, error=null); настройки восстановлены, pointer/EXE неизменны. Это
environment/workload abort, а не доказательство дефекта capture и не разрешение
считать ранний участок полным прогоном. Все файлы и отрицательный outcome
сохранены. Критерии A/V, mix и strict decode не ослабляются; полный повтор
перенесён после остальных native-проверок в model-free библиотеку.

## Итоговая UI-сборка и native Whisper

После native UI найден отдельный крайний случай: пустая ручная правка без
speaker создавала SRT cue без текста, который importer обоснованно отклоняет.
Corrected SRT теперь пропускает пустые/whitespace строки и нумерует оставшиеся
cue подряд; original export сохраняет исходные слова. `.bazza` по-прежнему
хранит пустую ручную правку. Шесть targeted export tests и typecheck/build прошли.
Новый EXE 28 564 992 B, SHA-256
`5164ab4969f55a39c34487bf3d9a78426704ec68d3bf2fb2a36c8941a05ff214`.
Native capture/ASR core относительно `c29013e9…` не менялся; предыдущий EXE
сохранён в `RUN2/builds/c29013e9f5e87f5c/`.

Шесть моделей взяты из ранее проверенного локального cache, без скачивания,
с проверкой размеров и SHA-256 (1 826 020 044 B). Для чистого ASR-стенда четыре
не-ASR модели временно удерживались вне library/models; Base/Small оставались
единственными доступными моделями. Настройки и список моделей не подменялись.
Первая попытка `native-asr-2026-11-02T00-20-36-251Z` остановилась до import из-за
полного набора моделей; это preflight abort, без processing.

`RUN2/native-asr-2026-11-02T00-21-01-618Z/` — **6/6 passed**:

- 12 с точного нулевого PCM: import Small, явный re-transcribe Base и Small —
  ни одного сегмента, complete/local provenance, исходный WAV неизменен.
- Фиксированный EN holdout: 0/25 word errors; RU: 2/23 substitutions,
  без insertions/deletions. Эти bounds совпадают с прежним fixture baseline
  и не оценивают речь реальных людей.
- JFK с gain 0,01 и паузами: два сегмента, тихая ненулевая речь сохранена;
  raw совпадает с выбранным provider transcript, времена в границах медиа.

Настройки восстановлены, pending processing нет, внешнего STT/CLI/capture в
этом ASR-тесте не было. После штатной остановки четыре удержанные модели
возвращены с проверкой hash для следующей сквозной проверки.

## Сквозная selected-STT приёмка

[Отдельный отчёт](2026-11-02-selected-stt-chain.md) подтверждает на итоговом
`5164ab49…` complete Soniox → native gallery proposal/explicit accept → имена и
ручная правка → local cleanup → local/Codex/Claude summaries → restart A →
`.bazza` import/restart model-free B. Raw/clean/manual flag/provenance/media hashes
сохранились; global voice identity/samples в B не переносились. В обоих player
после restart прошли три API seek и 20 переходов по репликам. Явный local
Re-transcribe отдельно подтвердил замену overlays и точную архивацию прежнего
cloud contract. A закрыта, исходные указатели/DB/WAL/SHM восстановлены.

## Полный длинный mic A/V-повтор: passed

Библиотека B:
`.git/local-tests/20261102-capture-reliability-20261102T002644-222976Z/`
(далее **B**). `B/long-capture-regression-7557af06-a656-4590-a18f-7cd755edf31e/`:
**39/39 technical checks passed**, итоговый `acceptance.json` связывает capture,
offline analysis и native reopen-player по SHA-256. EXE `5164ab49…` сохранён
в `B/builds/5164ab4969f55a39/`.

30 минут wall time, DISPLAY2 1920×1080/30, отдельные ненулевые mic и system
стимулы, четыре паузы, Stop на последней без Resume. Активное аудио 1735,106 с,
видео container 1735,132683 с. Все четыре файла полностью декодируются; frame PTS,
packet DTS и AAC PTS монотонны. Последний видеокадр находится за 7,047 мс до
конца аудио; первый video PTS 0,466016 с допускается заданным startup bound.
Это проверки именно непрерывного длинного сценария, не production-порог для
отклонения целого короткого/delayed video.

| Audio−video, мс | Начало | Середина | Конец | Размах медиан |
| --- | ---: | ---: | ---: | ---: |
| Mic WAV / mic в mix и AAC | +255,651 | +238,151 | +258,984 | **20,833** |
| System WAV / system в mix и AAC | +280,651 | +263,151 | +285,651 | **22,500** |

По 19/18/18 независимо сопоставленным маркерам в трёх окнах. Оба размаха ниже
заданного до теста лимита 40 мс. Начало→конец: mic +3,333 мс, system +5 мс.
Постоянный offset не компенсировался и включает раздельный запуск/рендер stimulus
players; его нельзя приписать приложению или переносить на настоящий созвон.
Предварительная calibration оригинальных/derived источников passed.

Mix сравнен с суммой 83 285 107 PCM frames. Максимальная разница **1 LSB**:
90 581 samples отличаются на единицу, выше единицы — ноль; bit-exact=false.
Это заранее объявленный эффект раздельного float→PCM16 усечения, не изменение
samples анализатором. Clipping отсутствует на всех трёх WAV. Более короткий sys
хвост (21 sample) дополнен нулями при сравнении.

180 capture samples покрывают около 99,44% интервалов. Известных посторонних
compiler/inference/FFmpeg jobs не наблюдалось. Средний Bazza-tree CPU 3,488%
всей 28-thread машины, пиковый private commit 1396,613 MiB. Это описательные
значения одного прогона; неизвестный фон/monitor overhead остаются, отсутствия
утечек и универсального performance threshold тест не доказывает.

После restart `B/long-reopen-player.json` passed: H.264/AAC video 1920×1080,
настоящее muted playback и три seek в начало/середину/конец. Сохранность ранее
импортированной встречи повторно проверена; поиск по её manual-only маркеру
находит исправленный сегмент при неизменном raw (`selected-cloud-B-manual-search.json`).

## Поздняя проверка недоступного PID

На `5164ab49…` обнаружен отдельный случай старта: несуществующий PID 4294967292
успешно активировал WASAPI virtual client, sys оставался точным нулём, Ready без
source warning. `B/per-app-unavailable-83930caa-4947-44c3-8816-bd98304c1bf9/`
остаётся отрицательным outcome, исходные файлы сохранены.

В `process_loopback.rs` добавлена только стартовая проверка: минимальное
`PROCESS_SYNCHRONIZE`, неблокирующий wait, закрытие handle по
[контракту WinAPI](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject).
Отсутствующий/уже
завершённый PID возвращает ошибку в существующий fallback; неопределённые ошибки
проверки, включая access denied, оставляют решение WASAPI. Mic/endpoint/video,
packet clocks и defaults не менялись. Independent review замечаний к итоговой
логике не обнаружило. Capture **83 passed / 5 ignored**, Clippy all-targets passed.

Итоговый EXE **28 566 016 B**, SHA-256
`a4da45014e989669212d80eecd1d10048426c4d3daa74ae3acc531505e7725bc`:

- `B/per-app-unavailable-31bf1d40-3120-4826-95b1-70bfdd177c93/` passed:
  недоступный PID даёт явное предупреждение и существующий fallback на выбранный
  системный endpoint; контрольная речь в sys (RMS 0,08210).
- `B/per-app-unavailable-594c5b2b-6e9e-44d9-b924-43b5aaed9b52/` passed:
  живой owned PID записывает речь (RMS 0,07687), fallback warning отсутствует.
  Native trace содержит 64 process packets; normal per-app path не заменён endpoint.

Настройки обоих повторов восстановлены. Поздний guard относится только к старту
per-app; прежний all-scope 30-минутный результат не объявляется запуском нового
EXE. Ongoing root death, surviving descendants и PID reuse/TOCTOU остаются
отдельными случаями, не решаемыми этой предварительной проверкой.

## Итог и оставшиеся проверки

A и B штатно закрыты. В B пять terminal-встреч, Recorder Idle, models=0,
voice profiles=0, private key отсутствует; port 9225 свободен. Контроллер подтвердил
побайтное восстановление указателей и неизменность пользовательских DB/WAL/SHM.
`B/final-delivery.json` фиксирует итоговый EXE и restoration evidence.

Открыты: native capture 44,1 kHz, безопасный default-input A→B→A, физическая
потеря/возврат endpoint, субъективное качество/реальный созвон, native Soniox
disconnect/reconnect и macOS. Одна чистая virtual input трасса не подтверждает
другую; render 44,1 kHz не считается mic 44,1 kHz. Контрольная человеческая EN/RU
запись ожидается 2026-11-03, Mac — на неделе. Общий этап продуктовой приёмки
остаётся открытым; ранняя отмена Claude оставлена по прежнему решению пользователя.
