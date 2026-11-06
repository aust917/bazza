# Полная автоматическая приёмка: Windows и искусственные данные

> Редакционная пометка: названия продукта, пакетов, команд и путей приведены к Bazza. Исходные результаты испытаний, измерения и хеши сохранены; при переименовании испытания не перезапускались.

Дата: 2026-11-02, Asia/Yekaterinburg (UTC+05:00). В JSON-доказательствах
используется UTC: основные испытания начались 2026-11-01 вечером UTC.
Статус: **доступный автоматический Windows-прогон завершён; продуктовая приёмка
не пройдена из-за обнаруженных отклонений**. Исходники приложения не менялись.
Продолжение [продуктового плана](../test-plans/2026-11-01-product-acceptance.md)
и [плана завершения](../plans/2026-11-02-automatic-acceptance-completion.md).

## Стенд и границы

Испытан существующий Windows EXE, без изменений исходников приложения:
SHA-256 `F0E88C04C63D965AFB80C6D4984D0954AE8108C4BAB889D6686821AC0982DBBC`.
Настройки записи и указатель библиотеки принадлежат одному lifecycle-контроллеру;
отдельные испытания восстанавливают свои настройки в `finally`.
Итоговая проверка указателя, остановки тестового приложения и ключа прошла ниже.

Native WebView2 CDP слушает loopback. Harness вызывает зарегистрированные
Tauri-команды, читает настоящие события, БД и медиа; playback/seek проверяются
через DOM/media API настоящего WebView. Только сами Windows file chooser flows
проверяются через Computer Use: настоящие диалоги, клавиатура и фокус поля.
Окна и видимый flash/tone-стимул размещены на DISPLAY2, 1920×1080,
physical position −1920/0, scale 1; primary DISPLAY1 — 2560×1440, position 0/0.
Это локальные автоматические acceptance-прогоны с native устройствами и моделями,
а не переносимые CI-тесты или browser mocks.

Библиотека и runtime-доказательства — игнорируемый каталог
`.git/local-tests/20261102-full-acceptance-204745/` (далее **FULL**).
Модели и SAPI TTS ground truth —
`.git/local-tests/20261102-model-acceptance-assets/` (далее **ASSETS**).
Пути ниже указаны относительно корня репозитория либо FULL/ASSETS;
медиа, модели, ключи, расшифровки и session state не включаются в Git.
Пользовательские встречи не используются. Soniox разрешён пользователем только
для искусственного аудио; агентные summaries используют выбранную подписочную
авторизацию. Неизолированная нагрузка всего компьютера отдельно не измеряется.

Предыдущие [720p/SRT/VTT](2026-11-02-automated-product-acceptance.md),
[`.bazza`](2026-11-02-bundle-runtime-acceptance.md) и
[устройства/A/V](2026-11-02-capture-device-acceptance.md) сохраняют свою область
доказательств. File chooser flow дополнительно проверен в этой задаче.

## 30 минут 1080p/30 FPS, паузы и Stop на паузе

Meeting `733e89d8-f9ba-45e9-9122-201249509e57`, попытка
`FULL/long-acceptance-2026-11-01T20-53-18-057Z/`.
Запись заняла 1800 с wall time; активное время по meeting — 1735,334 с,
MP4 — 1735,366 с. Четыре паузы: около 420–435, 900–920, 1350–1365 и
1785–1800 с; последняя закончилась Stop без Resume. Ready достигнут,
capture error отсутствует, исходные настройки восстановлены.

Все **14 технических проверок** `long-analysis.json` прошли: полное декодирование
mic/sys/mix/MP4, точное сложение mix, четыре паузы, ожидаемый формат,
возрастающие decoded video PTS, DTS обоих потоков и AAC PTS,
независимые first/middle/last A/V и замеры ресурсов по всей записи.
Декодированы 52 050 кадров 1920×1080, nominal 30 FPS, средний FPS около 29,998.
Медиана межкадрового интервала 33,333 мс, максимум 66,667 мс.
Проверка возрастающих timestamps не доказывает отсутствие пропущенных кадров.
На 83 296 064 сравниваемых samples mix точно равен mic+sys; clipping и ошибки
полного decode не обнаружены.

114 flash/tone-маркеров заданы нерегулярными интервалами и частотными ID;
111 пар независимо сопоставлены в видео и sys/mix/AAC. Паузы исключают
соответствующие исходные интервалы. Критерий выбран **до записи**:
размах медиан AAC onset − video onset first/middle/last не более 40 мс.
Это технический порог с учётом кадра 30 FPS (~33,3 мс) и шага детектора 5 мс,
а не универсальная граница воспринимаемого качества.

| Окно исходного стимула | Пар AAC/video | Медиана AAC − video |
| --- | ---: | ---: |
| 0–300 с | 20 | +63,659 мс |
| 750–1050 с | 18 | +61,992 мс |
| 1500–1780 с | 18 | +75,326 мс |

Размах медиан **13,334 мс**, first→last **+11,667 мс**: критерий 40 мс пройден.
Постоянный offset измерен и не компенсируется. Причины задержек между player,
endpoint и Bazza не разделены. Субъективное качество не прослушивалось и не принято.

Ресурсы измерены 180 samples на протяжении capture, шаг около 10 с.
Bazza-tree: средний CPU **3,181% всей машины**, пик **4,518%**;
максимальный private commit 1 803 550 720 B. В пик входят потомки Claude CLI
в трёх samples; это смешанная нагрузка, не чистый footprint рекордера.
Модели работали 20:54:38,820–20:55:42 UTC, CLI — 20:54:51–20:57:12 UTC;
после ~21:00 UTC были короткие cached helper builds без проигрывания аудио.
Поздний интервал после 21:10 UTC без этих известных inference jobs:
CPU средний 3,133%, пик 3,718%, private commit peak 1 539 710 976 B,
сумма working sets peak 1 640 337 408 B. Shared pages могут учитываться
в нескольких working sets; короткоживущие процессы между samples не охвачены.
Пользовательская/фоновая нагрузка host не изолирована. Offline decode выполнялся
после записи и не влияет на сохранённые capture-метрики; он мог влиять на latency
последующих параллельных inference-проверок.

Доказательства: `verification-summary.json`, `long-analysis.json`, `protocol.json`
и resource samples указанной long-попытки. Повтор требует подготовленного
FULL session и fixture: `node .git/local-tools/run-long-acceptance.mjs --session
<ABS_SESSION> --fixture <ABS_FIXTURE>`, затем
`python .git/local-tools/long-analyze.py --attempt <ABS_ATTEMPT>`.

## Отдельный 720p resource benchmark и picker

`FULL/benchmark-720-494417a1-fff2-4221-98c2-bb4ce50dc7c4/verification-summary.json`
— passed. Единственная новая запись `b4b21ecc-6c47-4a2d-b1f8-83a795d6d77f`:
120,423 с wall time, MP4 120,178646 с, 1280×720, nominal 30 FPS,
average ~29,992 FPS. Все четыре файла проходят strict full decode.
Mic точно нулевой, sys/mix содержат подготовленную речь через VM3;
native muted playback и API seek на 3/60/115 с после reopen прошли.

60 samples с шагом около 2 с покрыли 118,005 с интервалов между измерениями:
weighted Bazza-tree CPU **2,889% всей машины**, пик **3,876%**,
private commit peak **778 833 920 B**, summed working sets **1 004 056 576 B**.
В tree входят Bazza/WebView/encoder ffmpeg, stimulus players исключены.
Модели временно перемещены только из тестовой библиотеки; native logs подтвердили
пропуск обеих ASR passes, других inference jobs не было. После завершения все
шесть файлов моделей восстановлены. Host workload не изолирован, working sets
могут повторно учитывать shared pages; короткие процессы между samples не видны.
Это двухминутное измерение без порога performance-quality и без leak test.
Иные stimulus/duration/workload не дают считать его контролируемым сравнением
масштабирования 720p против предыдущих 1080p.

Настоящий popup WebView до записи и после reopen показывает сохранённые
**All apps on output** и точное имя VM3; Cancel оставляет idle и прежний список
встреч. FPS/720 в picker не показываются; соответствие настройке проверено
по MP4, отсутствие этих надписей не объявлено дефектом.

Исходный `result.json` остался промежуточным: runtime создал пустой `models/`,
и helper не смог вернуть прежний каталог поверх него (`EPERM`) до final flush.
Восстановлены исходные модели/указатель и отдельно проверен существующий результат,
без повторной записи. Прежние connection/settings-assertion failures harness
сохранены отдельно; сравнение settings исправлено с порядка JSON keys на semantic
equality. Итоговый `verification-summary.json` и `final-cleanup.json` passed.

## R1: микрофон и два системных приложения

Первый all-CABLE прогон `FULL/local-pipeline-2026-11-01T21-25-04-695Z/`
не подтвердил разделение: CABLE Input и CABLE In 16ch оказались слышны в обоих
архивных источниках. Он сохранён как неуспешный; результат не объясняется
предположением, что Bazza игнорирует endpoint. Взаимные шестисекундные пробы
подтвердили независимый маршрут **Voicemeeter In 3 → sys** и
**CABLE Input → CABLE Output mic**: неиспользуемый источник был точно нулевым.
Системный output ID —
`wasapi:{0.0.0.00000000}.{0ad75068-9214-4ddd-8ceb-234ade191e99}`;
Windows default output при этом не менялся.

Повтор `FULL/local-pipeline-2026-11-01T21-37-46-126Z/`, meeting
`b114ef6b-032c-46bc-a9a5-ba7a4833a5f7`: три разных owned процесса последовательно
играют David-a в mic, Zira-a и David-b в выбранный системный bus.
Scope — все приложения выбранного bus; process-loopback этим не принимается.
Перед стимулами все три WAV имели ровно нулевые первые 3 с.
Независимая корреляция с исходниками: **0,994916 / 0,991338 / 0,988501**,
против заранее заданных ≥0,98; wrong-track peak 0, mix равен mic+sys точно.
Фактические непересекающиеся gaps 3,182 и 3,295 с, больше требуемой 1 с.
Зафиксированы live captions до Stop, quality Whisper Small, local provenance,
raw text и допустимые временные границы; Ready и processing.done дождались.
Quality WER 2/74 = **2,703%**, live-before-stop WER **21,622%**.
Настройки восстановлены. Это результат на искусственном тексте, без общего
порога приёмки качества ASR.

**Сохранность микрофонной waveform не принята.** В повторе mic и mix содержат
одну резкую нулевую вставку 50 samples на 5,269604 с; в sys и reference её нет.
В первом прогоне подобных mic-вставок было 75. Высокая корреляция сама по себе
не снимает этот дефект. Сравнение независимого callback-order CPAL capture с
одновременным Bazza на коротком контроле: raw correlation 0,989701 и 0 вставок;
Bazza 0,886386 и 7 вставок. Один native underrun/overrun warning raw-контроля
сохранён. Доказательства — `r1-abrupt-zero-review.json` обеих R1-попыток,
`FULL/r1-routing-probes-2026-11-01T21-36-02-349Z/result.json` и
`FULL/r1-direct-correlation-review.json`.

Review-only proposal находится в
`.git/local-tests/20261102-microphone-fidelity-proposal/windows-native-microphone-review.patch`.
Он переводит Windows mic на существующий native frame-counter endpoint reader,
сохраняя выбранный вход, default buffer duration, default-follow и macOS CPAL.
На изолированной копии: 78 unit tests passed, 3 ignored; targeted mic 5 passed;
Clippy release/all-targets с denied warnings и `git apply --check` прошли.
Приложение не изменено, candidate hardware capture не запускался.
Детерминированный current-clock repro показывает вставку 300 и обрезку 250
samples из 3840 при QPC jitter; native positions сохраняют их точно.
Это проверяемый механизм, но реального packet timing trace нет, поэтому
происхождение каждой наблюдавшейся вставки остаётся ограниченным выводом.

## Локальные ASR, модели и Voices

Все шесть моделей сверены с SHA-256 каталога: Whisper Base/Small,
pyannote segmentation, TitaNet Small, BGE Small и Qwen3 1.7B Q4.
В `ASSETS/native-tests/results.json` четыре настоящих model-backed Cargo case
завершились exit 0, каждый с ровно одним passed test: Base/Small JFK fixture,
Qwen `generates_pong`, BGE `real_model_embeds_similar_texts_closer`.
Это проверки исполнения и ограниченного модельного контракта.

Native импорт offline TTS holdout через quality Small, отдельно EN/RU:

| Исходник | WER | CER без пробелов |
| --- | ---: | ---: |
| David EN, 25 слов | 0/25 = 0% | 0/134 = 0% |
| Irina RU, 23 слова | 2/23 = 8,696% | 2/146 = 1,370% |

Нормализация: Unicode NFC, case-fold, ё→е, пунктуация исключена;
лексические замены и порядок сохраняются. Score JSON был записан до ошибки
консольного CP1252 вывода символа ё; исправление затронуло только helper stdout,
исходный результат не пересчитан в благоприятную сторону.
Доказательства — `import-*-score.json` и `*-console-fixed.json` первой R1-попытки.
Native playback/seek EN/RU и persistence после restart прошли; см. раздел
«Сохранность после перезапусков» ниже.

Native CPU pyannote/TitaNet sidecar прошёл 11 synthetic cases: одиночные голоса,
два/три различных TTS-голоса, смешанный sample и silence; finite nonzero
192-dimensional embeddings для речи, silence без turns/embedding.
Holdout similarity: David 0,955005, Zira 0,966515; неизвестная Irina максимум
0,538519 ниже app threshold 0,62. Доказательство —
`ASSETS/native-diarization/summary.json`. Human DER и качество идентификации
реальных людей по синтетическим голосам не оцениваются.

Native Voices, `FULL/voices-native-1a2d811f-e4c3-41b1-a5c2-e8a9f1764c84/result.json`:
21 именованная проверка passed, настройки восстановлены. Созданы два профиля
с двумя samples каждый; embeddings конечны и нормированы; sample playback
проверен в WebView. Добавление неверного sample, его удаление вместе с аудио и
восстановление centroid прошли. Rename/restore прошли. Silence и смешанный
sample отклонены без вставки в БД. David/Zira holdout предложили ожидаемые
профили, Irina — unknown; предложения не назначили имя автоматически.

Полный native pipeline прошёл **15 проверок**, настройки восстановлены:
`FULL/full-model-pipeline-2e3b014e-7140-4eed-bb9d-dc7b32f671e0/result.json`,
meeting `189f59fa-0bb1-4ef7-822c-30fb4281c489`. На 72,865 с английского TTS
Whisper Small дал WER 2/152 = **1,316%**. Снимок до cleanup подтвердил
неизменность raw text/IDs/times/labels; cleaned text хранится отдельно.
Pyannote/TitaNet выделил два голоса и предложил два ожидаемых профиля,
не назначая имён. Для всех chunks BGE vectors конечны и нормированы.
Local Qwen summary и отдельно явно вызванный Codex summary сохранили Alice,
Friday, Bob, Carol; числовой бюджет не придуман. Это ограниченный factual check,
не полная семантическая оценка. Полный hash агентного summary сохранился после
явного local re-transcribe, cleanup и rename/re-embedding. После rename наблюдались
оба completion events, включая отложенный refresh через четыре секунды.
Для title guard только у этой искусственной встречи fixture установил
`title_auto=1` в одной строке БД; после reprocess заголовок остался прежним.

## Codex и Claude Code summaries

Реальные короткие и длинные synthetic inputs прошли через оба подписочных CLI.
Длинный rendered prompt — 73 004 bytes, проверены обязательные факты и tail marker.
Для обоих проверены job reopen, cancel в model phase и отказ при 2 100 780 bytes
(лимит 2 MiB) с сохранением предыдущего summary. Основные восемь выбранных
случаев passed; ранние harness/fixture failures сохранены в том же журнале.
Доказательство — `FULL/evidence/agent-acceptance/results.json`.
Повторная provenance/persistence после полного restart прошла, см. ниже.

**Дефект ранней отмены Claude сохранён:** на 175 мс во время auth check процесс
останавливается и старый summary сохраняется, но UI сообщает login failure.
Отмена на 1000 мс в model phase вернула корректный `Summary cancelled`.
Пользователь выбрал оставить ранний дефект в отчёте и не менять код.
Успешный поздний cancel не отменяет неуспешную раннюю проверку.

## Soniox и отдельные неуспешные границы

Первый native local preprobe, `FULL/soniox-native-b64d2efb-*/result.json`:
owned helper PID играл 10,125 с в независимый VM3 bus, StartOpts.audio_pid
указывал на него, headroom около 3 с. Получен sys WAV лишь ~26,375 мс,
полностью нулевой, тогда как mic/mix покрывают 13,758 с и также нулевые.
Fallback notice не наблюдался; cloud gate не пройден, платный happy-case
в этой попытке не запущен. Все-приложения VM3 работает; process-loopback
**не принят**. Возможные native position/duplicate handling причины требуют
bounded packet trace и пока не установлены.

Повтор Soniox на явно выбранном изолированном VM3 all-scope прошёл local-only
source gates до cloud: virtual mic оставался нулевым, owned speech correlation
0,998878. `FULL/soniox-native-18dc8fcf-*/result.json`: happy meeting
`67088639-a35a-4bcb-af9f-fb9b19cb011d`, 230 partial и 19 final событий,
19 подтверждённых реплик; synthetic WER 4/152 = **2,632%**.
Все четыре substitutions — запись числительных цифрами; эта метрика сохраняет
такое различие и сама по себе не означает четыре смысловые ошибки.
Медиана arrival − audio-end: partial 782,026 мс, final 759,526 мс;
включает buffering/transport/provider endpointing, порог качества не заявлялся.
После Stop Ready сохранён, но provider завершился с **invalid audio watermark**,
transcript status `incomplete`; happy complete check false, попытка **не passed**.
Живой и подтверждённый текст/медиа сохранены. Проверка адаптера отклоняет watermark
выше количества отправленных samples; конкретные значения не логировались,
причина превышения не установлена. EOS rounding/padding остаётся гипотезой.

Продолжение `FULL/soniox-native-13314cac-8478-46c7-971f-274237f51602/result.json`
повторно проверило изоляцию локально и использовало **ту же** happy-встречу,
без нового длинного платного запроса. Из 41 проверки **40 прошли**;
`happy_soniox_complete=false` сохранён, общий outcome остаётся отрицательным.
Неподдерживаемая модель дала настоящий HTTP 400, Ready/8,058 с локального медиа,
ноль подтверждённых cloud captions и отсутствие неявной замены Whisper.
Отзыв consent и удаление ключа на 20-й секунде отдельных 28-секундных записей
сохранили по четыре подтверждённых фрагмента. После остановки worker текст
не менялся; sys/mix продолжались, включая ненулевые последние две секунды.
Виртуальный mic во всех случаях оставался точно нулевым. Явный Small re-transcribe
первой happy-встречи завершился, сохранив прежний cloud transcript/provenance
и SHA-256 всех исходных медиа. Local WER 2/152 = 1,316% на этой записи.
Тестовый private key отсутствует, настройки восстановлены.

В local silence probe архив PCM независимо подтверждён точно нулевым,
но quality Whisper Small вернул `you`. Это отдельная наблюдаемая ошибка
модельного/postfilter результата, не доказательство постороннего аудио.
Источник безопасности проверяется PCM; неожиданное слово не засчитано как pass.
Настоящий transport disconnect/reconnect не выполнен: текущий Windows процесс
не elevated; сеть, firewall и глобальные устройства ради этого не менялись.
In-memory transport tests не заменяют native восстановление соединения.

## Настоящие файловые диалоги

На втором мониторе проверен Library **Media + transcript**: выбор control MP4,
затем SRT либо WebVTT с фильтром `Subtitles (UTF-8)`. Созданы две Ready-встречи:
`43430775-5f73-4383-b6c3-0053c926827d` и
`b635a6c4-3a3a-44b2-8647-4d1bac8cc49a`. Native IPC подтвердил точные три cues,
таймкоды и ожидаемые VTT speaker labels. Отмена первого и второго диалога
не изменила список встреч до параллельного Soniox continuation.

Settings **Import bundle** после настоящего выбора `valid.bazza` сообщил
`Imported 2, skipped 0 already here.`, повтор — `Imported 0, skipped 2 already here.`
Отмена не изменила список встреч. Через **Share → .bazza bundle** проверены отмена
и сохранение одной video-встречи. Архив 17 566 058 B: CRC исправен, пять entries,
четыре media hashes точно совпадают с оригиналом. Manifest сохраняет данные
встречи с ожидаемыми import transformations: source-local folder/voice bindings
очищены, IDs segments созданы заново. Ссылка для обмена не создавалась.
Доказательства: `FULL/evidence/chooser-ui.json`, `chooser-state.json`,
`chooser-export-verification.json` и игнорируемый `chooser-export.bazza`.

Computer Use UIA дважды вернул `element ... is not available in cached app state`
при заполнении модального file field. После обновления окна использованы
наблюдаемый фокус поля, клавиатура и проверка введённого пути; imports завершились.
Это ограничение автоматизатора сохранено отдельно от результата приложения.

## Crash, прерванная обработка и отсутствие CLI

`FULL/recovery-3397ffd8-d58f-4ecb-89dc-7d6efb8d9106/result.json`:
два из трёх сценариев прошли, crash-recording дал **degraded**, общий outcome false.
Принудительно завершён только проверенный собственный PID Bazza, без tree kill.
Его capture ffmpeg завершился через **843 мс**; собственные stimulus players
пережили kill, выбранные пользовательские GUI process identities сохранились.
Перед kill virtual mic проверен нулевым, sys/mix содержат искусственную речь.
RAW/WAV сохранены отдельно до startup recovery.

Crash meeting `a9c7cafe-d87e-4786-bab4-07f598dd0270` стал Ready с `error=null`.
WAV декодируется; объявленная длительность 25,2996 с, ещё 40 960 B находятся
за объявленным WAV data chunk после аварии (это верхняя граница, не принятые samples).
Native WebView воспроизводит запись и выполняет seek. Однако MP4 и сохранённый RAW
не проходят строгий полный decode: `Invalid NAL unit size (2172 > 1575)`.
Из 313 объявленных кадров декодируются 312; длительность video stream 10,4667 с,
AAC/container — 25,299 с. Следовательно, Ready и короткий playback не доказывают
полное восстановление видео. Повреждение уже присутствует в RAW; это не доказательство
причины потери всех исходных кадров. Ремонт/re-encode не выполнялся.
Bounded stderr/PTS/hashes: `crash-video-decode-review.json` в той же попытке.

Прерывание настоящего Processing при импорте 72,865 с искусственной речи прошло:
после restart Ready, 18 segments, действительный provenance `whisper-small`,
валидные времена, исходный файл не изменён; playback/seek прошли.
Отдельный child environment с PATH только System32 сделал оба CLI недоступными.
Ошибки Codex/Claude не изменили прежние summaries. Затем Bazza перезапущен с
нормальным PATH и прежними настройками. Глобальный PATH и установленный софт не менялись.
TerminateProcess здесь не моделирует внезапное отключение питания.

## Сохранность после перезапусков

`FULL/full-reopen-fb34bab8-5d09-4771-ab19-eff6890b9f1a/result.json` — passed:
все восемь сохранённых agent summaries и provenance точно совпали с последними
успешными ledger entries, активных jobs нет. Отдельный pipeline Codex summary
также совпал по canonical SHA-256. Настройки проверки остались неизменными.
Настоящий muted playback/API seek/row seek прошёл для **семи** записей:
EN/RU holdout, длинной 1080p, chooser SRT/VTT и chooser bundle audio/video.
Raw/clean представления и ожидаемые labels проверены, media hashes не менялись.
Профили David/Zira, два samples каждого и их playback прошли четыре reopen checks:
`FULL/voices-native-reopen-b0ab2375-7a8d-4f24-be1e-125310a4a719/result.json`.

Для полной pipeline-встречи отдельно прошли exact raw/clean UI, все 18 row seeks,
72,865 с audio player и native FTS `Alice Friday` (четыре hits, нужная встреча есть):
`reopen-player-search-32fa4f5f-efd8-42c7-bd96-b325defc6428.json` в её pipeline-каталоге.
Это проверка FTS; semantic retrieval отдельно не заявляется.

Неуспешные harness assumptions сохранены: первый reopen требовал равенства целых
source/imported MP4, хотя importer по контракту копирует H.264 stream и заново
кодирует AAC/remuxes. Исправлен только harness: source hash проверяется отдельно,
точный H.264 stream hash совпал, оба imported MP4 полностью декодируются.
Первая FTS probe искала отсутствующее в fixture слово `beta`; правильная probe
использует содержащиеся в ground truth `Alice Friday`. Эти ошибки проверки
не объявляются дефектами приложения, платная обработка ради них не повторялась.

## Итоговые отклонения и оставшаяся область

| Наблюдаемая граница | Результат |
| --- | --- |
| Mic PCM fidelity | Нулевая вставка 50 samples в изолированном повторе; причина не окончательно доказана |
| Per-app loopback на helper/VM3 | ~26,375 мс нулевого sys вместо проигранной речи; all-scope на том же endpoint работает |
| Whisper Small на точной тишине | Сохранено выдуманное `you` |
| Soniox Stop/finalization | Подтверждённый текст сохранён, но `invalid audio watermark`, incomplete |
| Ранняя отмена Claude | Данные сохранены, неверное auth-сообщение; пользователь отказался от исправления |
| Crash recording recovery | Ready без error при повреждённом/укороченном видео, strict decode failed |

Review-only mic proposal и его отдельные проверки описаны в разделе R1 выше.
Это **не изменение принятого EXE** и не аппаратная проверка кандидата.
Изменение приложения по результатам приёмки требует отдельного обсуждения.

macOS, физическое отключение endpoint, реальный transport disconnect/reconnect,
реальные люди/звонки, DER/биометрическая точность, восемь участников и субъективное
воспринимаемое качество остаются вне выполненного synthetic Windows-прогона.
Нагрузка измерена отдельно для 1080p и 720p в описанных выше разных условиях.
Общие гарантии отсутствия пропущенных кадров и устойчивости к power loss не даны.

## Завершение и проверки

`FULL/evidence/final-cleanup.json` — passed, 2026-11-01 22:07:39.589 UTC
(2026-11-02 03:07:39.589 местного времени): 37 тестовых meetings, нет Recording,
Processing или summary jobs. Soniox key отсутствует, provider local, consent false.
Тестовый Bazza завершён штатно, port 9224 закрыт; owned recording players/ffmpeg
завершены. Исходные байты `data-dir.txt` восстановлены, SHA-256 исходной `bazza.db`
совпадает с baseline. Во время reopen также не менялись её WAL/SHM.
EXE SHA-256 прежний, application source не изменён; секретный пользовательский
файл сохранён на месте и локально исключён из Git. Ничего не опубликовано.

После дополнительного 720p-прогона заключительный cleanup повторно прошёл
2026-11-01 **22:17:27.546 UTC** (03:17:27.546 местного времени), уже с 38 meetings:
`final-cleanup.json` в benchmark-каталоге. Настройки и шесть моделей восстановлены,
private key отсутствует, нет Bazza/owned capture players/ffmpeg, port закрыт,
исходные pointer bytes/DB hash и EXE hash прежние.
Независимый финальный read-only контроль дополнительно сверил SHA-256 всех
шести восстановленных моделей и повторил pointer/DB/EXE/no-Bazza/port checks:
`FULL/evidence/final-independent-verification.json` — passed.

Документация проверена `node scripts/check-docs.mjs` и `git diff --check`.
Runtime/model-backed проверки приведены выше; broad frontend/native rebuild
после документирования не требовался, поскольку приложение не изменялось.
