# Soniox → диаризация → ручные правки → перенос

Дата: 2026-11-02, Windows. Статус: **passed на искусственных материалах**.
[Roadmap](../plans/2026-10-30-local-fork-roadmap.md) ·
[Ручные исправления](../plans/2026-11-02-transcript-corrections.md) ·
[Capture и ASR](2026-11-02-capture-reliability.md).

## Проверенная граница

Standalone EXE SHA-256
`5164ab4969f55a39c34487bf3d9a78426704ec68d3bf2fb2a36c8941a05ff214`,
28 564 992 B, schema v8. Локальные модели ранее подготовлены и проверены по
размеру/SHA-256; в этом цикле они скопированы из cache без скачивания.
Выбран настоящий complete Soniox transcript 80 088 мс, 20 captions из
искусственного English dialogue. Контрольные материалы и обе библиотеки
находятся только в игнорируемой `.git/local-tests/`.

Библиотека A: `20261102-capture-reliability-20261101T231533-946850Z`.
Библиотека B: `20261102-capture-reliability-20261102T002644-222976Z`.
Meeting ID: `1bdbd857-edd1-48b9-8114-571680da3138`.
Private Soniox key удалён до начала этой цепочки; повторного cloud STT нет.
Codex/Claude вызваны явно только для саммари искусственного текста.

## Действия и наблюдения

| Граница | Наблюдаемый результат |
| --- | --- |
| Независимые voice samples | Два новых SAPI David WAV, 14,670 и 15,815 с, разные тексты/PCM; они не повторяют clips выбранного dialogue. Через native IPC создан один профиль с двумя управляемыми samples; Zira не добавлялась. |
| Post-call diarization | Локальные Pyannote/TitaNet дали несколько labels. IDs, raw-текст, времена и сохранённый Soniox contract не изменились. |
| Предложение и подтверждение | Имя не применено автоматически. Наблюдаемое предложение соответствует enrolled David; label неизвестного/смешанного источника известное имя не получил. Явный accept связал профиль, затем явная правка имени сохранила тот же profile ID. |
| Исправленный текст | Native empty → reset → nonempty manual correction сохранили raw/provider captions, время, имена и медиа. Ручная правка хранится только в очищенном представлении. |
| Cleanup и индексы | Локальный cleanup обработал остальные строки, сохранил manual overlay и исходный Soniox contract. Автоматическое переименование/подтверждение не запускало саммари. |
| Саммари | Явные local Qwen, Codex и Claude завершились с правильным provenance. Для внешних агентов проверены четыре доступных в тексте опорных слова и отсутствие выдуманного денежного бюджета; это ограниченная fixture-проверка качества. |
| Restart A | Raw/clean/manual flag, времена, segment IDs, labels, summary/provenance, media hashes, подтверждённая voice link и IDs двух независимых samples сохранились. |
| `.bazza` → B | Экспортирована одна встреча, импорт без ошибок. Повторный импорт пропускает существующую встречу. Raw/clean/manual flag, времена, labels, summary/provenance и media hashes точны; новые segment IDs сравниваются по переносимым полям. |
| Локальность B | Нет моделей и глобальных voice profiles; локальные identity links не переносятся. Import не запускал inference/summary и не менял выбранный Soniox contract на local. |
| Restart B и player | Данные сохранились; настоящее muted audio playback, три API seek и все 20 переходов по репликам прошли в A и B после restart. Продолжительность player 80,088771 с. |

Для действия accept использовалась заранее заданная защита: не менее пяти секунд
перекрытия с David reference и нулевое перекрытие с Zira. Это guard тестового
действия, не подобранный threshold продукта и не DER/биометрическая оценка.
Ни matching policy, ни материалы выбранной записи ради успеха не менялись.

## Явная повторная транскрипция

После экспорта отдельный native Re-transcribe с Whisper Small в A прошёл.
Он намеренно заменил сегменты, labels и ручные overlays; flags сброшены,
raw соответствует новой выбранной local-транскрипции. Прежний Soniox contract
сохранён точно в `stt_previous:<meeting>`, media bytes неизменны.
При выключенном автоматическом summary прежнее саммари сохранилось.
Экспортированный до замены пакет и библиотека B сохраняют исходную cloud-цепочку.

## Доказательства и завершение

В test root A:

- `selected-stt-gallery-enroll-8ccdb29e-fb00-418b-baae-a4c35a6de661/ledger.json`;
- `selected-stt-chain-2026-11-02T00-23-38-456Z/`: snapshots, native events,
  экспортированный пакет и полный ledger;
- `selected-stt-verify-reopen-2026-11-02T00-26-09-356Z/`;
- `selected-stt-explicit-retranscribe-2026-11-02T00-26-28-123Z/`;
- `selected-cloud-A-reopen-player.json`.

В test root B:

- `selected-stt-import-2026-11-02T00-27-44-033Z/`;
- `selected-stt-verify-import-reopen-2026-11-02T00-27-56-282Z/`;
- `selected-cloud-B-reopen-player.json`.

Настройки восстановлены после каждого сценария, listeners/собственный template
удалены, key отсутствует, незавершённой обработки нет. A штатно закрыта;
исходные указатели восстановлены побайтно, пользовательские DB/WAL/SHM неизменны.
B затем прошла полный model-free длинный capture, native search по ручной правке
и повторную проверку импортированной встречи. Она штатно закрыта, исходные
указатели/DB/WAL/SHM восстановлены; детали в
[capture-отчёте](2026-11-02-capture-reliability.md).

Это функциональная интеграция двух TTS-голосов, не приёмка реальных EN/RU людей,
DER, восьми участников, произвольных неизвестных голосов или macOS. Большой
[fixed voice suite](2026-11-02-voice-matching.md) имеет отдельные материалы,
метрики и snapshot; его выводы не подменяют результаты этой цепочки.
