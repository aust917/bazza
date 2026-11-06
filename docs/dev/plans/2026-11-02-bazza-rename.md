# Имена и форматы Bazza

Дата: 2026-11-02. Статус: complete — полное переименование, удаление совместимости,
согласованные проверки и общая Windows EXE-сборка завершены.

## Результат и границы

Имя продукта — **Bazza**, технические идентификаторы — `bazza` и `BAZZA_*`.
По последнему решению пользователя приложение использует только собственные
форматы и каталог данных. Исходники, документация и архив имеют единые имена.
Исключения: исходная MIT-лицензия и реальные ссылки на оригинальный проект.

Изменения остаются на этом компьютере. Публикация, перенос текущей рабочей копии,
удаление пользовательских библиотек и изменение capture defaults не входят в
задачу. Модели, аудио, секреты и игнорируемые runtime-доказательства не меняются.

## План и критерии готовности

- [x] Пакеты, imports, EXE/sidecar, переменные окружения, интерфейс, сайт и значки
  используют Bazza; собственные имена в скриптах и CI согласованы.
- [x] Portable-файлы имеют расширение `.bazza`, экспорт библиотеки —
  `bazza-library.bazza`. Manifest требует `app: "bazza"`; другие продуктовые
  идентификаторы и расширения отклоняются до записи данных.
- [x] Каталог по умолчанию — платформенный `Bazza`, база — `bazza.db`.
  Читается только собственный `data-dir.txt`. Альтернативный поиск каталогов,
  баз, import pending/staging-маркеров удалён.
- [x] Доступ к медиа ограничен текущими Bazza-путями и явно выбранной библиотекой.
  Импортный диалог предлагает только Bazza bundle.
- [x] Windows first-frame marker принимает только `bazza.clock=1`.
  Текущий macOS/no-clock timing path сохранён как отдельная платформенная ветка.
- [x] Архив и исторические отчёты нормализованы редакционно; числовые результаты,
  статусы и хеши сохранены, новые runtime-испытания этим не заявлены.
- [x] Исходная лицензия и upstream URLs сохранены. Унаследованный updater удалён;
  отдельный канал и публикация не настроены.
- [x] Выполнить согласованные проверки кода и финальный residual audit.

## Сохранность данных

Приложение не ищет и не преобразует файлы другого продукта. Удаление поддержки
не удаляет существующие пользовательские файлы. Выбранная Bazza-библиотека
использует `bazza.db` независимо от имени её папки.

Settings > Data > Change folder требует пустую невложенную папку без `..` и
создаёт согласованный `Store::snapshot_to` / SQLite `VACUUM INTO` снимок, включая
committed WAL. Исходная библиотека остаётся открытой и не переименовывается.

## Координация

- Эта задача владеет store `db/import`, share `bundle/staging`, точечными
  `organize.rs`, `tauri.conf.json`, скриптами и документацией переименования.
- Локализация удаляет старый импортный фильтр/переводы в Settings/locales и
  обновляет owning localization guide, сохраняя свои изменения UI/UiMessage.
- Mic/recovery-владелец обновляет first-frame marker parser/test в `video.rs`;
  он ведёт собственные capture/recovery изменения и отмену старой приёмки.
- Voices-владелец ведёт diarization/voice-store policy, corpus guide и первый
  согласованный targeted Cargo batch с `bazza-store` и `bazza-share`.
- Runtime и `target/release/bazza.exe` передаются между владельцами по очереди;
  новые сборки не перезаписывают EXE во время frozen runtime.

## Проверка

Первоначальный проход переименования до удаления совместимости: Windows CPU,
222 Rust tests passed / 9 ignored; 39 UI tests, TypeScript, Vite и Astro builds
passed. Затем локализация подтвердила 27 desktop tests / 1 ignored и 29 store
tests. Эти числа относятся к предыдущему состоянию и не заменяют новый прогон.

После удаления совместимости:

- Локализация подтвердила 59 UI tests и frontend build после удаления импортного
  фильтра и переводов.
- `cargo test --release --locked --offline -p bazza-detect -p bazza-capture --
  --test-threads=2` — 95 passed, 7 ignored, 0 failed.
- `cargo test --release --locked --offline -p bazza-store -p bazza-share
  -p bazza-diarize -- --test-threads=2` — 81 passed, 0 failed. Включены строгий
  формат bundle, snapshot/WAL, новые voice policies и защита export от сохранённого
  непригодного видео с `has_video=false`.
- `node scripts/check-docs.mjs`, `git diff --check` и `node --check` обоих изменённых
  `.mjs` scripts — passed.
- Residual audit по текущим source/docs/archive: непредусмотренных упоминаний
  прежнего имени нет; осталось 12 строк лицензии и реальных upstream-ссылок.

Первый desktop `--lib` run остановился на Windows sharing violation: Tauri build
script копирует resource DLL в release root даже при тестировании, а согласованный
recovery runtime удерживал `ggml-base.dll`. Повторный тест изолирован через
временный `TAURI_CONFIG` override с пустыми `bundle.resources/externalBin` и DLL
в `PATH` из `src-tauri/resources`; исходный config не менялся. Полный desktop
`--lib` run — **33 passed, 1 ignored**, включая data-move, voice matching, locale,
tray и video diagnostic tests.

После нового mic/recovery process-counter fix повторный `bazza-capture` run —
**79 passed, 5 ignored**. Вместе с актуальными detect/store/share/diarize и desktop
результатами это **210 passed, 8 ignored, 0 failed** в затронутых пакетах; это не
заявление о полном workspace run.

Cargo освобождён и передан локализации для одной общей EXE-сборки с исходной
конфигурацией и `tauri/custom-protocol`; она включает все согласованные source
изменения. Данные, текущий pointer и замороженные runtime-файлы эта задача не
меняла. Подробные журналы остаются в игнорируемом локальном tools.

После отдельного Voices review владелец повторно проверил store (37 passed) и
sidecar CLI (3 passed), Clippy для store/diarize/CLI с denied warnings — passed;
sidecar release build и staging — passed. Это отдельная проверка его последующих
provenance/JSON правок. Актуальная сумма указанных suite — 214 passed, 8 ignored.

Общий Windows EXE собран владельцем локализации с исходной конфигурацией:
`cargo build -p bazza --release --features tauri/custom-protocol --locked --offline`
— passed. Артефакт `target/release/bazza.exe`, 28 649 984 B, SHA-256
`1d79410c679a9474cfd01a47f96afefc70d1135d16a4ab3481ad4be229bd1727`.
Все strict-format изменения включены. Tauri Clippy all-targets с denied warnings
— passed. Тот же владелец подтвердил 10 native language/tray сценариев, затем
остановил приложение и восстановил pointer; последующую собственную приёмку
Voices/recovery ведут другие задачи. Это не повторная приёмка всех форматов/OS.

Независимый финальный аудит подтвердил 12 допустимых упоминаний в 10 файлах:
исходная лицензия и реальные upstream-ссылки/связанные с ними подписи. Остаточных
продуктовых упоминаний или файлов с прежним именем в source/docs/archive нет.

## Следующее действие

Обязательных действий по этой задаче не осталось. Не перезаписывать общий EXE
во время согласованной runtime-очереди Voices/recovery. Рабочую копию,
runtime-артефакты и чужие изменения не переносить и не откатывать.
