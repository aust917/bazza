# Микрофон и восстановление после сбоя

Статус: автоматическая Windows-часть complete; аппаратные/человеческие и macOS
проверки открыты. Пользователь 2026-11-02 поручил выполнить обновлённый
[roadmap](2026-10-30-local-fork-roadmap.md), начиная с этих двух исправлений.
Ветка: локальная `codex/pre-acceptance-2026-11-01`; общее рабочее дерево с задачами
переименования Bazza, русской локализации и подготовки голосового корпуса.

## Результат и границы

Сохранить микрофонный PCM без вставок/обрезки из-за packet QPC jitter;
повреждённое аварийное видео не должно выглядеть полностью восстановленным.
Основание — [полный отчёт](../test-reports/2026-11-02-full-automatic-acceptance.md).
Review-only mic candidate интегрирован и проверен коротким native контролем;
длительный контроль и аппаратные границы перечислены ниже.

Все изменения локальны. Capture, модели и внешняя обработка используют отдельную
тестовую библиотеку и контрольные материалы; рабочая пользовательская библиотека
сохраняется. Ранняя отмена Claude остаётся без изменения по решению пользователя.

## Владение файлами и ресурсами

- Основной исполнитель: microphone path в `crates/bazza-capture/src/stream.rs`,
  `process_loopback.rs`, crate `recorder.rs`; интеграция, проверка и этот план.
- Recovery path: `crates/bazza-capture/src/video.rs`, адресные изменения
  `src-tauri/src/pipeline.rs` и только orphan recovery в Tauri `recorder.rs`.
  Локализация передала эти Tauri-файлы; её `notice_message` и code/args сохраняются.
- Независимый mic fidelity harness подготовлен и испытан в игнорируемой области.
- ASR: точная тишина в `bazza-asr`, числовая диагностика и проверенное отделение
  terminal completion от replay ACK; nonterminal/timestamp guards сохранены.
- Переименование, локализация и Voices завершены; завершившим агентам сообщения
  больше не отправляются по уточнению пользователя. Cargo и общий runtime
  принадлежат основному исполнителю; EXE не меняется во время capture.
- Корпус Soniox готов; его frozen artifacts и вспомогательные скрипты принадлежат
  отдельной задаче. Генерация для повторных тестов не требуется.

## Критерии и шаги

- [x] Mic: перенести и проверить native capture endpoint без LOOPBACK,
  device-default buffer, default-input follow только при нефиксированном input;
  macOS CPAL path сохранить. Детерминированный jitter не меняет исходные samples.
- [x] Mic: выполнить native короткий контроль, повтор и pause/resume с независимым
  источником на virtual 48 kHz. Один полный pause/Stop-on-pause passed; остальные
  исходы и ограничения перечислены в отчёте, без объявления всех попыток passed.
- [ ] Mic: отдельно проверить native 44,1 kHz/default input/физическую потерю.
- [x] Длительный mic A/V с ненулевыми отдельными источниками: 30 минут,
  размах mic 20,833 мс / system 22,5 мс, четыре паузы и Stop на последней.
- [x] Recovery: строгая проверка RAW и временного MP4 до публикации восстановленного
  результата; повреждённые inputs и предыдущий хороший output сохраняются.
  Сохранившийся MP4 без RAW также проходит проверку при startup recovery.
- [x] Recovery: пользователь видит сохраняемую видеоошибку при частичном результате;
  доступное аудио и безопасный повтор startup recovery остаются рабочими.
- [x] Recovery: valid delayed video, короткое видео с полным аудио, паузы и Stop
  на паузе не отклоняются произвольным duration threshold. Отсутствующие исходные
  кадры и power loss не объявляются покрытыми тестом TerminateProcess.
- [x] Целевые crate/ffmpeg/model checks, Clippy затронутых модулей, затем native
  повторы после освобождения общего runtime.
- [x] Полный 30-минутный прогон после объединения: mic/sys/mix, четыре паузы,
  Stop на паузе, decode/PTS/tail, first/mid/last mic/sys/AAC A/V и ресурсы.
- [x] Per-app, Soniox и добавленные в roadmap задачи STT/диаризации/правок
  и сравнения видеонастроек прошли на искусственных материалах. Приёмка библиотеки прежнего
  бренда отменена новым решением пользователя об удалении старой совместимости.

## Проверки и передача

Mic, strict recovery, typed video diagnostics, process frame order и bypass точной
тишины реализованы. Актуальные адресные проверки: capture 83 passed / 5 ignored,
четыре FFmpeg fixtures, ASR 26 unit tests, по три Base/Small model tests, Store 48,
share 21, desktop 34 passed / 1 ignored, Clippy затронутых пакетов. Frontend:
полный набор 74 passed, после SRT fix шесть targeted export tests и production
typecheck/build passed. Версии этих проверок различаются; точные snapshot,
хеши и прежние отрицательные результаты сохранены в
[отчёте](../test-reports/2026-11-02-capture-reliability.md).

Private v7 EXE `839f12e7…` прошёл recovery A/B/C, AI failure/retry → restart
persistence, selected-PID speech и exclusion чужого процесса. После точного
Soniox terminal overrun 25 samples исправление прошло 26 unit tests и native
80 с complete. Сравнение 3×120 с видео passed. Native ASR — 6/6, v7→v8 и UI
correction passed; selected cloud → gallery accept → текст/cleanup/summary →
restart → model-free B перенос прошёл по
[сквозному отчёту](../test-reports/2026-11-02-selected-stt-chain.md).

На `5164ab49…` прошли полный 30-минутный capture и 39 offline checks, включая
mic/sys/mix, decode/PTS/tail, отдельные A/V-маркеры и ресурсы; native player/seek
после restart passed. Первый abort на 222 с сохранён отдельно и не принят.
После него недоступный PID обнаружил дополнительный startup edge: WASAPI
принимал отсутствующий target и писал нулевой sys. Стартовый guard в итоговом
EXE `a4da4501…` прошёл 83 capture tests, Clippy и native absent/live PID повторы;
all-scope/microphone/video path им не менялся. Ongoing descendants/PID reuse
не объявляются проверенными.

A и B штатно закрыты, исходные DB/WAL/SHM и указатели восстановлены. B остаётся
сохранённой model-free тестовой библиотекой, без key и global voice profiles.
Точные хеши, ограничения и оставшиеся проверки ведутся в отчёте; отрицательные
outcomes сохраняются. Далее — человеческая EN/RU запись и Mac в сроки пользователя,
native 44,1/default-input/physical endpoint и отдельный Soniox reconnect.
