# Дребезг системного звука: диагноз и исправление

Дата: 2026-11-01. Статус: программное исправление; новый аппаратный повтор ожидается.
[План](../plans/2026-11-01-system-audio-fidelity.md).

## Наблюдение и доказательства

Ручная приёмка EXE из `2772f8b`: NVIDIA Broadcast mic, Windows-default output
(Voicemeeter), все приложения, 1080p/30 FPS. Запись — 1026,605 с. Пользователь
слышит дребезг примерно с 12 с; микрофон до этого нормален. Ready и правильная
длительность не означали сохранность waveform; приёмка не пройдена.

В `sys.wav` найдены нули с резкими ненулевыми границами: 12,056458 с — 49 samples,
12,106458 с — 74, 13,016458 с — 98. На 10–40 с — 120 таких вставок с границей
более 100 PCM units; основные длины — 49 и 72–74 samples. В mic таких вставок нет.
`mix.wav` соответствует mic+sys с погрешностью максимум 1 PCM unit. AAC на 12–30 с
соответствует mix с cosine 0,99885, gain 0,99397; ошибок декодирования/clipping
не выявлено. Player использует один media element. Порча присутствует до AAC;
эти числа не доказывают идеального перцептивного качества кодека.

Внешнего оригинала и packet timestamp trace нет, поэтому точный список прежних
trim/insert операций этой записи восстановить нельзя. Исходные WAV/MP4 сохранены.

## Причина

`placement()` редактировал PCM каждого native-пакета по QPC: разница более
48 samples (1 мс) превращалась в нулевую вставку или обрезку. Колебание QPC
не является доказательством пропавших/повторённых кадров. cpal callback не
передавал native frame position и флаги для различения этих случаев.

Точная прежняя функция воспроизвела дефект на непрерывной синусоиде 997 Гц:
480-frame packets, чередование QPC на +73 samples, 999 правок за 10 с,
36 500 вставленных и 36 427 удалённых samples. Длительность — 10,00152 с,
SNR — −3,16 dB. Проверки длительности/таймкодов/отдельных маркеров этот дефект
не выявляли. Порог допуска увеличением не маскировался.

## Исправление

- Windows system loopback читает WASAPI выбранного endpoint с прежним native
  mix format. Mic cpal и macOS сохранены. Windows-default следует смене выхода;
  явно выбранный endpoint остаётся выбранным endpoint.
- Один начальный Unix origin и native frame counter заменяют покадровое
  выравнивание по QPC. Непрерывный PCM не режется из-за jitter. Настоящие
  frame gaps/overlap обрабатываются до SRC.
- SILENT — нули независимо от buffer contents/null pointer. Недостоверное время
  отмечается оценочным; discontinuity/reset сохраняют принятый PCM.
- Короткие реальные gaps проходят SRC как нули; длинные используют sparse
  interior с bounded filter edges и абсолютной дробной фазой.
- Event handle совместно принадлежит owner/worker; Stop/COM cleanup выполняются
  и на ошибках. Отменённый старт не запускает запоздавший client.

Контракт: Microsoft [GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer)
и [buffer flags](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/ne-audioclient-_audclnt_bufferflags).

## Проверки и сборка

- `cargo test --release -p hark-capture -p hark --lib --locked --offline`:
  **76 capture + 20 Tauri passed**, 0 failed; 3 capture и 1 Tauri fixture ignored.
- `cargo clippy --release -p hark-capture -p hark --all-targets --locked --offline -- -D warnings`: passed.
- `tauri build --no-bundle` с baseline override прямого TypeScript/Vite: exit 0.
  `target/release/hark.exe`: 29 815 296 байт, SHA-256
  `F0E88C04C63D965AFB80C6D4984D0954AE8108C4BAB889D6686821AC0982DBBC`.

Три теста реального WAV writer сравнивают каждый i16 sample raw и mix с
неповторяющимся LCG + 997/1733/4523 Hz reference: разные packets, jitter,
gaps/duplicates/reset. Native helpers проверяют decoder/SILENT, 17/49-frame gaps,
overlap, uncertain time, event lifetime, partition independence и sparse SRC
против эквивалентного непрерывного входа с нулями на 44,1→48 кГц. Независимый
review выявил и устранил reset/uncertain/SRC-boundary и resource-lifetime ошибки.

Остаётся прежний bounded EOF-tail fractional SRC: Recorder при Stop уже закрывает
callbacks, поэтому около 16 native frames lookahead могут не попасть в архив.
Текущий 48 кГц passthrough этим не затронут; stop/recovery ради этого отдельного
случая не переделывались. Реальные A/V, idle/pause и device recovery требуют
повтора после изменения native system path. Старые измерения не являются новыми.

Новая архивная аппаратная запись не запускалась. Fingerprints, численная
диагностика и прежний EXE находятся только в игнорируемой локальной области.
Речь не транскрибировалась и внешним сервисам не передавалась.

Исправленный EXE перезапущен с той же тестовой библиотекой. Library показывает
прежнюю Ready-запись 17:06; read-only DB проверка — одна Ready, ноль
recording/processing. SHA-256 всех четырёх исходных WAV/MP4 совпал с сохранённым
до исправления. Settings сохраняет NVIDIA Broadcast, System default output,
all-app scope и Russian. Это проверка запуска/сохранности, не качества захвата.
