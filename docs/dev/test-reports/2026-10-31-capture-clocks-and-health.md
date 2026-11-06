# Общие часы захвата и доступность экрана: контрольная точка

> Редакционная пометка: названия продукта, пакетов, команд и путей приведены к Bazza. Исходные результаты испытаний, измерения и хеши сохранены; при переименовании испытания не перезапускались.

Статус: **проверка не завершена; работа остановлена по просьбе пользователя**.
Продолжение — в [активном плане](../plans/2026-10-31-capture-clocks-and-health.md).
Новая реализация не объявляется полностью проверенным исправлением остаточного offset.

## Что изменено локально

- Аудио получает время первого sample из native capture clock, с явной оценкой
  при недоступной метке. Общий origin создаётся до открытия устройств; worker
  сохраняет `mix.clock.json`. Учтено время retained history ресэмплера.
- Windows/gdigrab сохраняет Unix PTS первого кадра в `screen.raw.clock.txt`.
  Mux использует разницу origins аудио/видео и перебазирует паузы. Постоянная
  поправка на 300 мс не вводилась. macOS-видео и старые записи сохраняют legacy-путь.
- Исправлены разрывы/повторы native аудиопакетов, поздний первый system packet
  после паузы, повтор старого PCM при degraded catch-up, callbacks/errors старого
  устройства после reopen. Ошибка WAV worker прекращает приём PCM.
- На время видео удерживаются Windows DisplayRequired/SystemRequired через
  owned handle с освобождением при stop/drop/сбое. План питания не меняется;
  ручной сон и блокировка не обходятся. Проверяется доступность desktop.
- Stderr FFmpeg читается отдельным потоком; сохраняется хвост до 64 KiB,
  проверяются выход процесса и timeout. Ошибка сохраняет исходники/метаданные.
  Tauri сообщает предупреждение, а `has_video` определяется готовым файлом.
  Ошибка видео сохраняется и после необязательных стадий обработки.

## Выполненные проверки

| Проверка | Результат |
| --- | --- |
| `cargo test --release -p bazza-capture --offline --locked` | 56 passed, 3 ignored; предыдущий запуск выявил устаревший сценарий теста, который обновлён под явный measured gap |
| Ignored tests с `BAZZA_TEST_FFMPEG=C:\ffmpeg\bin\ffmpeg.exe` | Все 3 passed: native power handle/release, first-frame marker + задержка видео 500 мс, pause mux/recovery |
| `cargo clippy --release -p bazza-capture --all-targets --offline --locked -- -D warnings` | Passed |
| TypeScript / Vitest | Typecheck passed; 21 тест в 7 файлах passed |
| Tauri `build --no-bundle` | Passed; EXE SHA256 `E477E4360909A0ADD09B48121535CA6F97086B1009F414B6F122AA7296D9CF0E` |
| Документы / diff | `check-docs`: 42 Markdown-файла, budgets OK; `git diff --check` passed |

Чистые/FFmpeg-тесты проверяют знак offset, сохранение раннего аудио, паузу до
первого кадра, открытые паузы, совместимость legacy, сохранение хорошего результата
при ошибке и ограниченный размер диагностики. Они не заменяют аппаратную приёмку.
После последних конкурентных поправок следует повторить согласованный набор
capture tests/Clippy перед продолжением аппаратных испытаний.

## Короткие реальные записи до остановки

Артефакты только локально:
`.git/local-tests/20261031-014043-clocks-health/`.

Временный Rust harness вызывает настоящие `Recorder`, `VideoRecorder` и
`mux_recording`. Это проверка capture crate с устройствами, **не сквозной запуск
нового desktop EXE**. Микрофон и выбранный системный output — по умолчанию;
монитор 0, 15 FPS, max height 1080. Источник — прежний синтетический AV control.

| Папка | Дополнительная задержка видео | `V−A` по clock-файлам | Длительность mix |
| --- | --- | --- | --- |
| `delay-0` | 0 мс | 125.265 мс | 22.131166667 с |
| `delay-500` | 500 мс | 620.660 мс | 22.626104167 с |

Оба harness завершились успешно, mux выполнен, timeline содержит
`capture_error:null`. На экране через UI наблюдался контрольный ролик. Разность
измеренных origins — 495.395 мс; **итоговая A/V дельта ещё не проанализирована**.
Успешный выход mux не доказывает отсутствие рассинхронизации или чёрных кадров.

В обоих прогонах сохранены предупреждения `A buffer underrun or overrun occurred`
и `Device capture timestamps are unavailable; recording timing is estimated`,
`estimated_timestamps:true`. Нужно установить источник/момент fallback и его
влияние на результат до вывода о точности аппаратного выравнивания.
Разборчивость человеческой речи не проверялась.

## Что осталось и как продолжать

1. Повторить capture tests/Clippy для сохранённого исходного состояния.
2. Численно разобрать `delay-0`/`delay-500` существующим
   `.git/local-tools/analyze-recording.py`; выяснить причину estimated timestamps.
   Проверить, одинаков ли результат A/V при дополнительной задержке старта.
3. Создать новую изолированную сессию, запустить сохранённый desktop EXE и проверить
   реальные записи без пауз, с несколькими паузами и stop во время паузы.
4. Выполнить подготовленный 370-секундный idle-прогон. На этой машине AC display
   timeout — 300 с. Fixture `stimulus-long/control.mp4` имеет длительность 390 с;
   её калибровка нашла все 26 маркеров, median delta −3.333 мс. Пока это проверка
   источника, а не успешная длинная запись Bazza.
5. Проверить предупреждение и сохранение файлов при реальном отказе видеопроцесса,
   затем обновить итоговые числа, ограничения и статус приёмки.

К моменту остановки Bazza, capture harness, FFmpeg/FFplay не работают; debug listener
9222 отсутствует. Новый desktop EXE в этой сессии не запускался, тестовая библиотека
не содержит встреч. Временный `%APPDATA%\Bazza\data-dir.txt` и пустой созданный
каталог удалены: восстановлено исходное отсутствие обоих. Тестовые файлы оставлены
в `.git/`; при возобновлении старый pointer автоматически не восстанавливать.
Публикаций, push, облачного STT и модельных вызовов не выполнялось.

## Основания решений

- [CPAL InputStreamTimestamp](https://docs.rs/cpal/0.18.2/cpal/struct.InputStreamTimestamp.html):
  capture и callback — разные моменты; задержка доставки callback не является временем sample.
- [FFmpeg 8.0.1 gdigrab](https://raw.githubusercontent.com/FFmpeg/FFmpeg/n8.0.1/libavdevice/gdigrab.c):
  PTS пакета задаётся через `av_gettime()` до BitBlt;
  [metadata filter](https://www.ffmpeg.org/ffmpeg-filters.html#metadata_002c-ametadata)
  позволяет сохранить первую метку отдельно от нормализованного media timeline.
- [Microsoft PowerSetRequest](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-powersetrequest):
  для удержания дисплея одновременно нужны DisplayRequired и SystemRequired.
  Связь прежнего чёрного видео именно с idle-off остаётся гипотезой до длинного прогона.
