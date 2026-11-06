# TitaNet: численная устойчивость и проверка противоречий внутри кластера

Дата: 2 ноября 2026 года. Windows, Intel i7-14700KF, 64 GB RAM. [План](../plans/2026-11-02-voice-reliability.md), [исходная приёмка Nemotron](2026-11-02-nemotron-integration.md).

Численный fallback восстановил **6/6 невалидных codec-векторов** и сохранил **64/64 исходных валидных вектора**. Однако неизменный matcher принял **0/6 восстановленных codec-запросов**: доступность конечного вектора не означает улучшение узнавания имён. Отдельная ограниченная проверка локальных окон убирает наблюдавшееся ошибочное mixed-предложение, сохраняя положительный результат человеческой contextual-сцены. Она пропускает некоторые примеси другого голоса и не доказывает чистоту всего кластера.

Отдельные слои доказательств — численные эксперименты, финальный CLI, native policy replay и приёмка настоящего приложения — перечислены ниже. Их результаты не подменяют друг друга.

## Данные и воспроизводимость

Основная численная матрица содержит 30 сцен, 70 анонимных speech pools и 434 запланированных локальных окна. Использованы точные WAV и интервалы реальных запусков финального предыдущего CLI: три codec-подкаста, чистые/шумные/overlap сцены, человеческие контрольные сцены и дополнительные DEV/source-heldout конструкции. Исходные кластеры не очищались по истинным speaker IDs. Один pool собирает первые не более 60 s речи исходного кластера; локальные coherence-окна выбираются отдельно по полной временной шкале кластера.

Численная политика использовала только длительность, конечность результата и полноту покрытия. Actor labels, ожидаемые имена, близость к gallery и желаемое принятие неизвестного не участвовали в выборе восстановления. Reference labels находятся в отдельных файлах оценки. Окна и разбиения повторно используют исходные записи, поэтому количества строк не являются числом независимых людей или испытаний.

| Неизменный артефакт | SHA-256 |
| --- | --- |
| TitaNet small ONNX, 40257283 bytes, 192 измерения | `ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e` |
| Sherpa C API DLL численного эксперимента | `8cf3a55962a9c820e3df50ae2ad20d3be49fc05b753392dcb3c6756731057649` |
| Nemotron Q8, 107012128 bytes | `08456d9e22cd9a323c0364d98375f3746d6e68507ebb705cd46438c534c7a3a1` |

Сохранены существующие artifact, frontend, PCM16 `/32767`, embedding space, профили и identity-пороги. Изменение `/32767` на `/32768` было только диагностикой: в 626 строках duration/normalizer grid не произошло ни одного изменения finite/invalid. Нормализацию production не меняли. Внутренний оператор, вызвавший NaN, не установлен.

Все аудио, модели, базы, vectors и журналы находятся в игнорируемых локальных каталогах. Аудио не отправлялось внешним сервисам. Пользовательская библиотека и обычное приложение не использовались для этих экспериментов.

## Замороженная численная политика

`tita/recovery-policy-v1.json` заморожен после validity-only pilot, до полной матрицы и проверки имён. Последующее разрешение production-реализации не меняет исторический статус этого файла.

1. Один раз вычислить исходный полный pool. Валидный нормализованный вектор сохранить точно, без усреднения и повторного выбора.
2. Только при невалидном полном результате и pool не короче 6 s разбить его на `n = max(2, ceil(N / 480000))` частей с границами `i*N/n`. Максимум части — 30 s, минимум — 3 s; ни padding, ни повторов, ни пропуска хвоста.
3. Вычислить все части. Восстановление допускается только при валидности каждой части и 100% покрытии исходных samples без пересечений. Каждая часть хранит точные spans исходных speech intervals, в том числе при переходе через разрыв исходной записи.
4. Нормализовать каждый вектор, усреднить все векторы с весом по длительности и снова L2-нормализовать. Ошибка хотя бы одной части оставляет query отсутствующим.

Нет выбора наиболее похожего окна, retry по имени, фильтрации «неудобного» голоса или изменения enrollment. Невалидные source spans, неверная длина чтения, NaN, нулевая норма и ошибка inference сохраняются как явные причины отказа. Coherence-окна проверяются отдельно; восстановленный общий вектор не исправляет невалидные локальные наблюдения.

Реализация ограничивает pool и метаданные: production pool до 60 s, транспортный предел pool 120 s, не более 65536 source intervals, 64 evidence clusters и 16 coherence-окон на кластер. Фактическая модель имеет 192 измерения; транспорт не принимает размерность выше 4096. Эти ограничения не заменяют проверку корректности каждого интервала.

## Численные результаты

| Проверка | Результат |
| --- | --- |
| Исходные полные pools | 64/70 finite; 6/70 invalid |
| Fallback на шести invalid pools | 6/6 finite, каждый из двух полных 30 s окон |
| Покрытие fallback | 960000/960000 samples для каждого pool, без пропущенного хвоста |
| Уже валидные исходные vectors | 64/64 сохранены исходными, без подмены диагностическим пересчётом |
| Исходные coherence-окна 5 s / minimum 3 s | 361/434 finite; 73/434 invalid |
| Идентификация восстановленных codec queries неизменным Rust matcher | 0/6 принятых имён |

Для шести codec pools проверены prefix/middle/suffix одинаковой заданной длины. Конечность не монотонна по длительности:

| Длительность | Finite codec-окна |
| --- | --- |
| 3 s | 0/18 |
| 5 s | 1/18 |
| 10 s | 1/18 |
| 15 s | 0/18 |
| 30 s | 18/18 |
| 60 s | 0/6 |

Поэтому правило «любое более короткое окно исправляет NaN» не подтверждено. Для чистых и человеческих контролей все доступные probes 3/5/10/15/30/60 s были finite. Пересчёт 64 валидных полных vectors через диагностический C API имел cosine с сохранённым CLI baseline не ниже 0.9999999999986228; максимальная покомпонентная разница — 3.90×10⁻⁷. Для проверки качества использовались исходные baseline vectors, а не эти пересчёты.

Детерминированный dither с RMS-relative амплитудами `1e-5`, `1e-4`, `1e-3` и пределом одной PCM16 LSB исследован отдельно. Каждая амплитуда исправила **0/73** исходных невалидных coherence-окон и **0/23** невалидных duration probes. Dither отвергнут и в production не применяется. Исходные PCM не менялись; увеличение амплитуды не проводилось.

Четыре неизвестные codec-подписи без имени не считаются четырьмя успешными open-set отказами. Их общий запрос теперь finite, но локальные окна остаются невалидными, а matcher не принимает восстановленную identity. Численное восстановление не устраняет смешение людей в исходном кластере.

## Проверка реального нового CLI

Выполнены парные отдельные процессы старого sidecar `f058dbb8…78e` и нового `9e94431c…0481` на трёх точных импортированных codec WAV, двух человеческих сценах и чистом подкасте двух известных голосов. Копии EXE/DLL запускались из собственного каталога, CPU affinity — два логических ядра, TitaNet — два потока. Использована предварительная coherence-геометрия 5 s / minimum 3 s, поэтому этот запуск не объявляется проверкой финального min1 naming gate.

- Все шесть запусков завершились успешно; turns старого и нового CLI совпали точно.
- Все 6/6 ранее отсутствовавших codec queries восстановлены. Metadata подтверждает обе 30 s части и полный source coverage. Cosine с независимо сохранённым Python aggregate не ниже 0.9999999999999998; максимальная разница компоненты — 7.43×10⁻⁹.
- Все 7/7 валидных human/clean primary vectors побитно совпали со старым CLI; fallback на них не вызывался.
- Невалидные локальные окна сохранены в evidence, а не удалены из знаменателя.

| Парный вход | Старый CLI, s | Новый CLI, s | Добавка, s |
| --- | --- | --- | --- |
| Codec, два известных | 16.881 | 19.823 | 2.942 |
| Codec, четыре известных | 17.028 | 18.389 | 1.362 |
| Codec, known + unknown | 16.428 | 18.637 | 2.208 |
| Human, anchors + returns | 5.610 | 5.949 | 0.339 |
| Human, short-only | 3.183 | 3.337 | 0.154 |
| Чистый подкаст, два известных | 16.832 | 17.739 | 0.907 |

Это одиночные локальные наблюдения с прогретым файловым кэшем после hash reads, а не контролируемый throughput benchmark. Они включают все draft coherence-окна, а не только fallback. Codec peak sampled RSS вырос с 845.0–851.6 MiB до 996.5–999.3 MiB; на human/clean прирост составил 5.1–25.4 MiB. В полной диагностической матрице выполнено 1728 фактических inference и 109 cache hits за 202.118 s; peak sampled RSS — 1069707264 bytes. Дополнительные две fallback inference на codec pool занимали медианно 501.87 ms, диапазон 488.49–566.00 ms.

## Проверка противоречий и финальный кандидат

Первоначальный DEV содержал ноль ошибочно принятых cluster proposals; выбирать на нём обоснованный pairwise cosine threshold невозможно. После этого фиксированные структурные варианты и искусственно merged controls исследованы явно как post-hoc диагностика. Они не объявляются слепым тестом, DER Nemotron или независимой калибровкой unknown identity.

Выбран инженерный кандидат `titanet-pool30-window5-tail1-rank-veto-compat-v2`, описанный в `coherence/deployment-candidate-v2.json`. Окна до 5 s берутся внутри исходных turns без padding и без склейки разрывов; minimum — 1 s. По полной шкале исходного кластера выбираются максимум 16 окон с детерминированным распределением по четырём clock quarters. Должны быть представлены все quarters, содержащие eligible окна. Доля непокрытой/subsecond речи учитывается отдельно; полного покрытия каждой секунды кандидат не обещает.

Неизменный aggregate `best_match` остаётся единственным источником предложения имени. При двух и более совместимых профилях положительный однозначный nearest-profile rank каждого локального окна должен согласовываться с остальными и принятым aggregate ID. Rank может отменить предложение, но не создать отсутствующую identity. Любое невалидное запланированное окно, противоречие или malformed evidence запрещает имя и pooled reuse; невалидные окна нельзя выборочно отбросить.

Для полного reusable proof нужны минимум два окна и корректная геометрия. С одним профилем относительный rank неинформативен: для reuse каждое окно должно пройти прежний identity verifier. Объявленная compatibility-ревизия сохраняет только уже существующее предложение валидного **PRIMARY** aggregate при недостаточном числе окон или недостаточной single-profile verification без наблюдаемого противоречия. Такое предложение остаётся pending; pooled query не передаётся в reusable voice memory. Восстановленные aggregates этой льготы не получают. Отсутствие gallery не создаёт имени и не разрешает pooled reuse. Явная регистрация из файла/sample остаётся отдельным путём.

| Повторно используемая диагностическая сцена | До: правильные / ошибочные cue assignments | Кандидат v2: правильные / ошибочные |
| --- | --- | --- |
| Human, anchors + short returns | 36 / 0 | 36 / 0 |
| Чистый подкаст двух известных | 12 / 0 | 12 / 0 |
| Чистая mixed-сцена | 2 / 4 | 0 / 0 |
| Human DEV full turns | 42 / 4 | 32 / 2 |
| 40 отдельных полных входов, multi-profile gallery | 18 / 0 | 18 / 0 |

Human-строка включает **6/6 anchors отдельно от 30/30 коротких известных возвратов**. Все 15 коротких неизвестных возвратов остаются без имени. Три gallery-разбиения повторяют одних трёх актёров и те же WAV; множество подписей использует один cluster query. Это сохранённая регрессия контекстного поведения, а не 30 независимых коротких voice-verification trials. Short-only узнавание этим изменением не исправлено.

Mixed-сцена содержит **одно** ошибочное предложение s2, повторно применённое к четырём неправильным и двум правильным подписям. Veto убирает все шесть привязок: цена удаления четырёх ошибочных — потеря двух правильных. В Human DEV остаются две ошибочные подписи неизвестного r2 как известного r3: одинаковый ближайший профиль во всех окнах не доказывает, что неизвестный человек стал известным.

В 13 явно injected merged-cluster controls чистый A сохранён; все **6/6 примесей B по 5 s** заблокированы, включая позднюю примесь после prefix60. Все **6/6 примесей B по 0.5 s** пропущены: они короче локального minimum, и имя A/reuse могут остаться ошибочными. Это намеренно собранные controls, а не шесть установленных ошибок Nemotron. Ограничение остаётся после выбранного исправления.

Отдельная post-hoc проверка 35 существующих middle crops из 14 TEST sources (2/3/5 s; семь недоступных source-duration bins без padding) выявила цену строгого min2 gate. В multi-profile gallery исходные правильные имена `0/26`, `2/24`, `8/20` строгий вариант сократил до `0/26`, `0/24`, `2/20`; compatibility v2 сохранил исходные `0/26`, `2/24`, `8/20`. Для single-profile full-isolated positives: 18 исходных → 0 strict → 18 compatibility. Эти сохранённые pending suggestions не дают права автоматически учить pooled voiceprint. Compatibility может сохранить и прежнее ложное pending имя; это явный предел выбранного компромисса.

Экспериментальный evaluator выполнил 6035 сравнений неизменного Rust `best_match`, расхождений с Python — 0; 2271 проверка hash совпала. Coherence/compatibility gate в этой серии исполнялся Python replay: эта цифра не является проверкой финального Rust `assess_cluster`. Сравнения повторяют запросы и galleries и не являются 6035 независимыми проверками точности.

До первого actual-app quality run зафиксировано дополнительное консервативное условие production: при единственном rankable профиле для reusable query должен пройти также aggregate `best_match`. Оно строже формулировки исторического frozen v2; ни одно автоматическое имя этим условием не добавляется. На исследованных singleton controls reusable queries были равны нулю и до этого уточнения. Валидный PRIMARY без полного подтверждения может сохранить только прежний pending результат без обучения.

Отдельный финальный native-policy replay вызвал именно production Rust
`voice_policy::assess_cluster`: 542 observation/gallery trials и один заранее
объявленный synthetic singleton edge. Для `naming_allowed`, `reusable`,
`verified_id` и unchanged global `best_match` расхождений с independent Python
v2 + явным addendum не было. В edge aggregate `[.8,.6]` не проходит verifier,
локальные A `[1,0]` проходят: historical v2 разрешал reuse, final source возвращает
`CompatibilityOnly`, `verified_id=None`, `reusable=false`. Никакое имя не повышено.
Helper собран offline в отдельном pure workspace/target без изменения корневого
lockfile. Source SHA `79b4be6330e6df3b7739191405b1c75642befd361db65b66712d6ad63b92037b`;
доказательства `coherence/assess-replay/summary.json`, input/output JSONL и
`source-policy-addendum.json`. Это 543 вызова policy на сохранённых наблюдениях,
не 543 независимые проверки качества и не app/CLI end-to-end.

Финальный CLI `e8a5931b18d0caac687ac668063d526ba9a046422f8a0f7e44d7e631b6eb56d2` повторил те же шесть пар со старым `f058dbb8…78e`, уже с production min1 и финальным policy ID. Turns совпали во всех шести записях, все 6 codec globals восстановлены с точным source mapping, 7 валидных PRIMARY vectors сохранены побитно. Независимо пересчитанный план совпал для всех 165 scheduled окон; 57 codec-окон невалидны и сохранены явно. Human/clean невалидных окон не дали. Эти результаты относятся к CLI/evidence, а не к UI или именованию. Журналы: `tita/final-cli-check/{summary,audit}.json`, `invalid-window-rows.jsonl` и per-case stdout/stderr. Одиночная добавка времени при affinity на двух логических ядрах и TitaNet threads=2: codec 1.052–2.415 s, human 0.356–0.656 s, clean 0.808 s; параллельная сборка/проверка приложения не позволяет считать это контролируемым benchmark.

## Локальные доказательства

Корень `.git/local-tests/20261102-voice-reliability/` содержит:

- `tita/recovery-policy-v1.json`, `dither-policy-v1.json`, `matrix-run-summary.json`, `numerical-recovery-analysis.json` — замороженные правила, hashes и численные проверки.
- `tita/pool-vectors.jsonl`, `recovery-window-vectors.jsonl`, `recovery-vectors.jsonl`, `coherence-vectors.jsonl` — все запрошенные строки, vectors или явные invalid reasons; source provenance хранится в manifest.
- `tita/production-draft3-check/audit.json` и per-case `output.json` / `stderr.log` — парные CLI проверки и точные EXE/input hashes.
- `coherence/pool-window-manifest.json`, `input-manifest.json`, `selection-procedure*.json`, `fresh-gallery-audit.json` — источник интервалов, правила выбора и fresh enrollment без meeting observations.
- `coherence/combined-results.json`, `combined-integrity.json`, `deployment-candidate-v2.json`, `structural-variants/scores/`, `compatibility-v2/`, `candidate-limits/` — policy parity, denominators, неудачные варианты, injected controls и ограничения.

Исходники helper находятся в `.git/local-tools/20261102-voice-reliability/`. Сами WAV, embeddings, базы и native build outputs в Git не включаются.

## Реализация и сборки

Изменены только voice/evidence границы: новые pure `voice_evidence.rs` и
`voice_policy.rs`, общий protocol, переиспользуемое ранжирование в `identify.rs`,
новый CLI adapter `voiceprints.rs` и Nemotron extraction в `main.rs`, native
mapping в `pipeline.rs` и bounded rename-cache hunk в `commands/speakers.rs`.
`smoke.rs` читает тот же новый policy-versioned cache key. Каталог моделей,
TitaNet artifact/frontend, `CURRENT_VOICE_MODEL` и identity thresholds не менялись.
Store/schema, MCP service/API, recorder, STT и capture defaults не редактировались
этой задачей. Обычное приложение и пользовательская библиотека не перезапускались.

Кэш reusable queries теперь хранится под
`speaker_embeddings:<policy_id>:<meeting_id>`. Старый unversioned cache не
используется для обучения. Повторная обработка сначала очищает текущий cache с
проверкой результата; конечные записи speakers/cache также проверяются.
Переименование переносит только наблюдаемый query и не подставляет centroid
сохранённого профиля. Объединение labels удаляет оба query до Store rename;
повторное naming не может зарегистрировать смешанный вектор. Это disposable
настройки встречи; исходные глобальные profiles/samples не мигрируются.

Сборки и библиотеки изолированы в `.git/local-tests/20261102-voice-reliability/`.
Каждая принятая runtime-копия содержит 51 файл с SHA/размером, полный отдельный
NeMo runtime и licenses. Все 50 файлов кроме app EXE совпадают между вариантами.

| Вариант | App SHA-256 | Статус |
| --- | --- | --- |
| `acceptance/runtime.next1/`, `app.bazza.voice.reliability` | `53f197a8ecfe9a857e75a82c5dd9b53d01650b6c7ecc274994f5fd77134cb113` | Packaged actual-IPC приёмка ниже |
| `runtime-production.next1/`, `app.bazza.desktop` | `28ddee188317b6e799059a98e731161e0cad500b94cf2e83d0db2506e3164174` | Собран из того же source; normal library не запускалась |
| Обоим: `bazza-diarize.exe` | `e8a5931b18d0caac687ac668063d526ba9a046422f8a0f7e44d7e631b6eb56d2` | Финальные 6 CLI pairs |

303 source/asset hashes обоих manifests совпали с финальным рабочим деревом.
Проверка `final-integrity.json` подтвердила 14/14 неизменных файлов обычного
`target/release` и одинаковую семью runtime libraries. Пользовательский GUI PID
81460 сохранился. Нормальный data-dir pointer не менялся. Текущий shared source
включает MCP Store schema12; fixture clones schema9 обновлялись только внутри
собственных тестовых библиотек. Старые schema9 EXE не запускались против schema12.

Первоначальные raw-Cargo `acceptance/runtime/` и `runtime-production/` не включали
`tauri/custom-protocol` и ожидали Vite. Они отвергнуты, quality cases на них не
выполнялись. Единственный такой owned процесс остановлен после OS PID/path/start
time/EXE-hash проверки; пользовательский процесс не затрагивался. Принятые next1
собраны с `--features tauri/custom-protocol`; actual startup проверяет
`tauri.localhost`, native identifier, private data dir и idle recorder.
Tauri staging может перезаписать target CLI старым `externalBin`, поэтому CLI
пересобран отдельно, заморожен вне target и включён в оба runtime по exact hash.

## Автоматические проверки

- `cargo test -p bazza-diarize --release --locked --offline -- --include-ignored`:
  58 passed, включая Python planner golden из 70 случаев.
- CLI: 20 обычных tests passed; отдельный opt-in test с настоящими GGUF/runtime/WAV
  прошёл повторные jobs, reset, обработку новым engine и native destruction.
  Первый общий запуск с `--include-ignored` без model env ожидаемо не прошёл
  setup; повтор с тремя явными локальными путями прошёл.
- `cargo test -p bazza --lib --release --locked --offline -- --include-ignored`:
  53 passed, включая FFmpeg import/reopen, original-cluster veto, merge/missing
  evidence, cache invalidation, rename и повторное naming.
- Pure/CLI Clippy `--all-targets --release -- -D warnings` и native
  `--lib --release --features tauri/custom-protocol -- -D warnings`: passed.
- `pnpm build`: TypeScript и production Vite build passed. UI source этой задачей
  не менялся; unrelated full frontend suite не перезапускался.
- Documentation checker и `git diff --check` passed; финальный прогон после
  завершения отчёта фиксируется ниже.

Автоматическая проверка отклонила предложенное удаление устаревших Hark-файлов
из собственного disposable target с причиной `blocked by policy`, без пояснения.
Ничего не удалено, повторных попыток не было; сборка завершена без этой очистки.

## Приёмка настоящего приложения

Packaged next1 прошёл 24 записанных run-группы и 270 assertions во всех четырёх
изолированных библиотеках. Это реальные Tauri IPC/events и SQLite snapshots
результатов; browser mock, ASR и запись с устройств не использовались. Run-группы
включают lifecycle/checkpoint/visual действия, а не 24 независимые quality scenes.
Исходные WAV/SRT hashes проверялись перед обработкой; reference speaker IDs
использовались только в отдельном scoring, не в inference или выборе окон.

Независимая повторная агрегация raw `get_meeting`, fixture truth и SQL snapshots
подтвердила 24/24 run-группы, 270/270 assertions и 26 processing jobs по 14
уникальным встречам. Repeated healthy, Me, negative и confirmation jobs исключены
из основной quality-оценки. Доказательства `acceptance/summary-aggregate.json` и
`coherence/actual-app-independent-cue-audit.json`; итог не основан только на counts
самого driver.

| Контроль в приложении | Результат |
| --- | --- |
| Чистый подкаст двух известных | 12/12 правильных pending привязок, два reusable queries |
| Чистый подкаст четырёх известных | 0 имён: существующий пропуск известных остаётся |
| Mixed known/unknown | Прежнее s2-предложение снято, reusable cache пуст; сняты также две правильные привязки того же смешанного кластера |
| Три codec-подкаста | Успешная диаризация, без имён и reusable queries; finite recovery отдельно доказан тем же CLI выше |
| Human context, три gallery rotations | 30/30 известных short returns сохранены, 15/15 неизвестных short returns без имени; anchors считаются отдельно |
| Human short-only, те же rotations | 0/30 известных имён; 15 неизвестных без имени. Улучшение short-only не заявляется |
| Long 1813.5 s, 26 повторов | 44.158 s, 468 captions сохранены, 312 known correct / 156 unknown withheld; stable actor-label mapping во всех блоках |

Каждый quality run сохранил исходный текст, timestamps и существующие profiles /
sample vectors. Names оставались pending до явного подтверждения. R1 context
обрабатывался с исходным meeting ID `27c1…`; Store исключал его собственный ранее
подтверждённый пятый sample по provenance. Ничего из этой базы не удалялось ради
лучшего качества. R2/R3 quality измерялись на исходных двух file refs на профиль,
до deliberate lifecycle mutations. R1 short-only и long используют сохранённую
пятую observation другого meeting: это отмечено как gallery caveat, не fresh test.
Три rotations повторяют тех же людей и аудио; подписи с общим query не являются
независимыми verification trials.

Long проверяет функциональную устойчивость на повторяющейся конструкции, не DER
естественной 30-минутной встречи. Все 26 блоков сохраняют 15 short events и
`r1/r2/r3 → Speaker 1/2/3`. Во время job выполнено 168 настоящих
`recording_status` IPC probes, максимум — 4 ms; recorder оставался idle.

Финальный тот же CLI отдельно обработал exact long WAV за 45.684 s при полной
affinity 28 logical CPUs / TitaNet threads=2: 753 turns, 181351 committed frames,
три валидных PRIMARY queries по 960000 samples. Все 48 локальных окон finite,
по 16 на кластер; независимо пересчитанный min1-план совпал для всех 753 turns.
У каждого кластера представлены четыре clock strata, поздние окна заканчиваются
на 1802.55 / 1807.79 / 1813.05 s. Их source clock существенно позже bounded
первых 60 s речи, заканчивающихся на 200–211 s. Recovery для long не понадобился;
его full-coverage mapping проверен на codec выше. Peak sampled RSS — 903921664 B.
Это фактическая raw metadata проверка, отдельно от app IPC.

Полнота временных четвертей не равна полноте речи: 16 выбранных окон покрыли
33.45 / 45.25 / 46.55 s из 510.88 / 542.58 / 539.17 s речи кластеров — примерно
6.55 / 8.34 / 8.63%. Subsecond tails и невыбранная речь остаются вне этих
наблюдений. Доказательства `tita/final-cli-long/{audit,output}.json` и stderr;
повторяющийся источник не является независимым качественным benchmark.

Отдельные end-to-end проверки защиты памяти:

- Старый unversioned cache действительно существовал в copied historical meeting.
  Naming к существующему профилю не добавил observation и не изменил его vector.
- Трёхсекундный CompatibilityOnly query сохранил прежнее pending имя, но не cache.
  Accept и два последующих display renames добавили ноль samples и не изменили
  исходные profile vectors.
- Eligible query после explicit accept добавил ровно один sample: его raw vector
  hash совпал с настоящим cached meeting query. Все исходные samples сохранены.
- Merge двух labels удалил source и target queries; последующее naming не
  добавило sample. Уже разрешённая исходная observation сохранилась.
- Me shortcut проверен положительным и отрицательным контролем: производные
  fixture mic/sys PCM сделали чистый Eligible cluster владельцем, но тот же метод
  не дал owner label конфликтному mixed cluster. Ни owner label, ни reusable
  query не обошли veto. Это искусственные tracks без hardware capture/hooks;
  исходное отсутствие mic/sys и mix hash восстановлены.

Missing и same-size corrupt TitaNet сохранили три анонимных Nemotron speaker rows,
captions и пустой query cache. Перед malformed Nemotron отдельно выполнен healthy
job с тремя cached queries; fault дал видимую optional-stage error и очистил cache,
сохранив прежние attributions/text. Это непосредственно проверяет preclear на
неуспешном job. Все исходные model bytes после fault восстановлены по SHA, здоровая
обработка прошла снова. Missing Nemotron завершил явный legacy fallback; его
quality остаётся прежним: одна wrong-known и одна false-known-for-unknown caption
в этом контрольном run. Защита нового Nemotron evidence не объявляется исправлением
legacy diarizer/enrollment quality.

Все четыре библиотеки прошли startup schema9→12 и restart retention. Сохранены
10 исходных profile IDs/model identities и все 21 исходных sample UUID/model/raw
embedding hashes. В R2 добавлен ровно один разрешённый actual-query sample; всего
после этого 22 samples. На restart сравнивались полный checkpoint profiles/samples,
pending/confirmed speaker rows и meeting details. Проверены hashes 12 исходных
model files и 79 media/sample files. Исторически ошибочные samples автоматически
не очищаются: сохранение базы не является повторной сертификацией её качества.

В actual bundled UI открыты Meeting и Voices: pending Yes/No имена и четыре
исходных профиля с двумя samples отрисованы, DOM и screenshots просмотрены.
Settings/recording actions не выполнялись. Owned app/controller после проверки
корректно остановлены; обычный GUI и data-dir pointer не затронуты.

Основные доказательства: `acceptance/app-final-summary.json`, `states/*.json`,
`runs/<library>/<phase>/result.json`, per-case before/after/score/events/SQL snapshots,
`negative-*/independent-fault-cache-clear.json`, `me-veto-*` и `visual-*` screenshots.
Raw rejected globals и subwindows не раскрываются обычным app IPC: численную
конечность и точную геометрию нельзя выводить только из пустого UI cache.

Финальные documentation/whitespace/source-runtime checks выполнены после
завершения приёмки. Итог двух разрешённых пунктов: conditional numerical recovery
и bounded contradiction/memory guard реализованы и прошли описанные регрессии.
Готовая normal-identifier сборка лежит в `runtime-production.next1/`; открытое
пользовательское приложение продолжает работать на прежней сборке.

macOS runtime/app для этого изменения не запускался. Естественные длинные
многоголосные встречи, независимые enrollment sessions и полнота обнаружения
subsecond/непросмотренных примесей остаются непроверенными границами. Два неверных
Human DEV unknown assignments и все шесть 0.5 s injected intrusions остаются
известными counterexamples; этот follow-up не объявляет универсальную чистоту
кластеров или максимальную достигнутую точность узнавания людей.
