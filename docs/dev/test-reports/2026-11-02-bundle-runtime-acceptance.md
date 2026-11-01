# Автоматическая Windows-приёмка `.hark` между библиотеками

Дата: 2026-11-02. Продолжение [приёмки SRT/VTT и 720p](2026-11-02-automated-product-acceptance.md)
и [плана импорта](../plans/2026-11-01-bundle-and-transcript-import.md).
Результат: подготовленный пакет перенесён между двумя изолированными библиотеками
через настоящий native IPC; отрицательные случаи, retry, duplicate, playback и
restart проверены автоматически. Код приложения не менялся. SHA-256 EXE:
`F0E88C04C63D965AFB80C6D4984D0954AE8108C4BAB889D6686821AC0982DBBC`.

## Стенд и контрольный пакет

Две новые библиотеки подготовлены offline с актуальными SQL-миграциями:
source содержит две Ready встречи, target первоначально пуста. Встречи и все
текстовые данные искусственные. Уже проверенные flash/tone WAV/MP4 скопированы
из контрольного прогона; новая запись, модели и внешние провайдеры не запускались.
Саммари, cleanup и сохранённое Soniox-происхождение — **fixtures**, не результаты AI.

Первая встреча содержит mix/mic/sys, видео 1280×720, три raw-реплики,
два отличающихся clean-текста, имена, структурированное/Markdown-саммари,
два тега, три highlights и сохранённое происхождение трёх final captions.
Вторая содержит три аудиодорожки и те же виды метаданных без видео/clean-текста.
В source также добавлены локальная папка, искусственный voice profile/sample,
ссылки/предложения голосов и заведомо фиктивные private/API-key markers.

`export_meetings` с `includeVideo: true` экспортировал две встречи:
**30 674 386 байт, ровно 8 ZIP entries** — manifest и семь разрешённых media files.
Для каждого файла SHA-256 source→ZIP совпал. Raw/clean, времена, metadata,
summary, имена тегов и highlights точно совпали с fixture и source БД.
Provenance сохранён только в разрешённых полях; fake API-key extras и private
settings отсутствуют. Embeddings, voice sample и лишний `voice-reference.wav`
в пакет не попали. Локальные folder/voice references в manifest не создают
соответствующих связей в target: при импорте они сбрасываются.

## Native импорт и восстановление

Прогоны выполнялись последовательно в одной target library. Проверки БД открывали
SQLite read-only, сверяли integrity/FK, данные, FTS/chunks, media hashes и отсутствие
оставшегося staging. Состояние фиксировалось детерминированным logical hash.

| Случай | imported / skipped / errors | Наблюдаемый результат |
| --- | --- | --- |
| Manifest version 999 | IPC rejection | Пустое состояние и контрольные файлы не изменились |
| Объявленный `../outside-bundle-fixture.wav` и соответствующая ZIP-запись | IPC rejection | Пустое состояние и внешний sentinel не изменились |
| CRC повреждён в mix первой встречи | 1 / 0 / 1 | Audio-only встреча импортирована; первая отсутствует, staging удалён; `Invalid checksum` |
| Путь первой встречи занят каталогом с контрольным файлом | 0 / 1 / 1 | Ошибка установки после начала metadata transaction; БД точно равна состоянию после CRC, посторонний файл сохранён |
| Корректный пакет после удаления только тестового препятствия | 1 / 1 / 0 | Добавлена недостающая video-встреча, сохранена ранее импортированная |
| Повтор корректного пакета | 0 / 2 / 0 | Данные и media hashes не изменились |
| Полный перезапуск Hark | — | Тот же logical hash, данные и media hashes сохранены |

CRC и занятый каталог проверяют разные границы: первая ошибка возникает при
копировании staged media до metadata transaction, вторая приводит к откату
уже начатой SQLite-транзакции. Вторая ошибка сообщила, что существующий каталог
сохранён. Version/traversal были отклонены; текст их native ошибки harness
сохранил только как общий `Uncaught (in promise)`, точная формулировка не принята.

В target точно сверены исходный/очищенный текст, timestamps, имена, summary,
highlights, теги и sanitized STT provenance. `folder_id`, voice links и suggestions
пусты, глобальные профили/образцы и private settings отсутствуют. Проверены
поиск raw/clean tokens, retrieval chunks и FTS. Числовые segment/tag IDs могут
генерироваться заново; они не должны совпадать с source.

## Настоящий player и завершение

В native WebView до и после restart проверены обе встречи:

- У video-встречи Raw/Cleaned переключаются и показывают точный fixture-текст;
  audio-only встреча содержит только raw. Имена и таймкоды сохранены у обеих.
- Видео декодируется как 1280×720 / 45,602 с; audio-only player — 45,516 с.
- Muted playback продвигается примерно на 1,06 с, у видео растёт счётчик кадров.
- Media API seek на 2/20/40 с и обработчик каждой строки транскрипта работают.

Приложение запускалось с CDP только на loopback; окна позиционировались на втором
мониторе через native API, физический указатель не использовался. В `finally`
все собственные экземпляры завершены, CDP listener закрыт, прежние байты указателя
библиотеки восстановлены. SHA-256 прежнего `hark.db` не изменился. Независимая
проверка доказательств подтвердила snapshots, retry/duplicate/restart и восстановление.

## Повторение и границы

Локальный стенд и JSON-доказательства остаются в игнорируемой области.
Подготовка — `seed-bundle-runtime.py`, запуск — `run-bundle-automated.mjs`;
`bundle-runtime-lib.mjs`, `bundle-fixtures.py` и `verify-bundle-runtime-data.py`
в `.git/local-tools` обслуживают IPC/player, ZIP mutations и read-only сверку.
Для нового прогона создаётся новая пара библиотек; существующий экспорт не затирается.
Основные evidence: `runtime-results.json`, `export_integrity.json`,
`target_after_*.json`, `mutation-*.json`, `playback_before_restart.json` и
`playback_after_restart.json`. Медиа, тексты fixtures и runtime state не коммитятся.

Это Windows-проверка данного EXE и синтетического пакета около 31 MB, а не portable
CI-тест или приёмка всех кодеков/размеров. Native file chooser UX, субъективное
качество/полное прослушивание, macOS, process crash/power loss и очень большие
пакеты открыты. Готовность реальных STT/саммари/голосовых моделей не проверялась.
