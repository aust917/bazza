# Soniox: завершение потока и progress watermark

Дата: 2026-11-02. Статус: исправление, 26 unit tests, Clippy и native 80-секундный
повтор прошли. Настоящий reconnect остаётся открытым. Продолжение
[roadmap](../plans/2026-10-30-local-fork-roadmap.md) и исходного
[автоматического отчёта](2026-11-02-full-automatic-acceptance.md).

## Наблюдаемый повтор

80-секундный контроль на замороженной Windows v7-сборке
SHA-256 `839f12e7bcd2defbfb3d2e367565e2edef9aa2435c431e194d2a659dd810d97b`
повторил `invalid audio watermark` после Stop. Два локальных source gates
прошли до первого обращения к Soniox; использованы искусственная речь,
отдельная библиотека, CABLE mic и выбранный VM3 bus. Сохранены 20 подтверждённых
реплик, итоговый статус `incomplete`; результат остаётся неуспешным.

| Поле терминального ответа / локального состояния | Значение |
| --- | ---: |
| `sent_samples`, `sent_relative_samples` | 1 280 615 |
| Длительность переданного PCM при 16 kHz | 80 038,4375 мс |
| `offset_samples` | 0 |
| `final_audio_proc_ms` | 80 040 |
| Пересчёт progress в samples прежним кодом | 1 280 640 |
| Превышение над переданным PCM | 25 samples / 1,5625 мс |
| `buffer_base_samples` / `buffer_end_samples` | 1 207 680 / 1 280 615 |
| `finished`, `eos_sent`, `input_closed` | true / true / true |

Локальные доказательства, не включаемые в Git:
`.git/local-tests/20261102-capture-reliability-20261101T231533-946850Z/soniox-stop-probe-2026-11-01T23-49-52-304Z/result.json`
и `launches/fixed-core-before-models.stdout.log` в том же test root.
Настройки восстановлены, presence тестового private key после очистки false.
Это устанавливает превышение терминальной статистики, но не его внутреннюю
причину: rounding, padding и размер модельного блока остаются гипотезами.

## Контракт и исправление

[Официальный WebSocket API](https://docs.soniox.com/api-reference/stt/websocket-api)
описывает пустой text frame как EOS, а `finished=true` — как завершение сессии.
`final_audio_proc_ms` описывает обработанное в окончательные tokens аудио;
точная sample-level граница или числовой допуск для округления не определены.
Официальный JS SDK отдельно
[ожидает finish после отправки EOS](https://github.com/soniox/soniox-js/blob/cf53c1d6605ebcb327253998cfebea6af686f028/packages/core/src/realtime/stt.ts#L341-L365)
и [завершает ожидание по `finished`](https://github.com/soniox/soniox-js/blob/cf53c1d6605ebcb327253998cfebea6af686f028/packages/core/src/realtime/stt.ts#L501-L548).

Выбранное решение в `crates/bazza-asr/src/soniox.rs` разделяет две роли:

- Обычный progress управляет replay buffer. Прежняя строгая проверка не допускает
  acknowledgement выше числа отправленных samples, в том числе после EOS,
  пока `finished` ещё false.
- Терминальный ответ завершает поток только при уже отправленном EOS, закрытом
  входе и отсутствии неотправленного PCM. Replay buffer освобождается ровно до
  локального `sent`, без преобразования терминальной статистики в ACK и без clamp.
- Ошибки провайдера и времена всех captions проверяются до завершения. Ответ
  `finished` без локального EOS или с неотправленным вводом вызывает retry.
  Строгая проверка caption end относительно отправленного аудио не изменяется.

Это адаптация семантики завершения, а не вывод о конкретном алгоритме округления
Soniox. Терминальное превышение остаётся ограниченной числовой диагностикой без
аудио, текста, credentials или необработанных server messages. Raw-медиа и
подтверждённый provider text этим исправлением не переписываются.

## Проверки и оставшаяся область

Пройдены regression cases: точные числа наблюдаемого повтора и экстремальная
терминальная статистика; nonterminal overrun; отсутствие EOS/закрытого ввода или
наличие неотправленного PCM; socket completion и отказ при неверном caption end.
Существующая privacy-проверка 101 мс progress при 100 мс PCM сохраняет отрицательный
nonterminal outcome. Проверки выполнены после окончания согласованного benchmark
quiet window, без моделей и запросов к Soniox.

| Проверка | Фактический результат |
| --- | --- |
| `cargo test --release -p bazza-asr --lib --locked --offline` | 26 passed, 0 failed |
| `cargo clippy --release -p bazza-asr --all-targets --locked --offline -- -D warnings` | passed |
| `node scripts/check-docs.mjs` и whitespace check | passed |

Логи остаются в `.git/local-tools/soniox-finalization-{tests,clippy}.log`.
Регрессия неверного terminal caption проверяет end 102 мс при 100 мс PCM:
`finished` не обходит timestamp guard. Для наблюдавшихся 1 280 615 samples тот же
guard допускает caption end только до 80 039 мс; end 80 040 мс останется ошибкой.

Native повтор на EXE SHA-256
`c29013e9f5e87f5cc0bfe82fb1fcb7eff9290723199ff8ecb855a603ce4d4885`
(28 564 992 B): **passed**, 80 088 мс, 20 подтверждённых captions, provider Soniox,
статус complete. Source correlation 0,99024045, local silence/source gates прошли
до чтения ключа. Настройки восстановлены, private value отсутствует после cleanup.
Доказательства в той же RUN2 библиотеке:
`soniox-stop-probe-2026-11-02T00-08-19-093Z/`.
В новом native прогоне terminal-overrun warning не наблюдался; точная прежняя
граница 25 samples закрыта воспроизводящим transport/unit case, а не утверждением
о повторном появлении этих же чисел у сервиса.

Если превышение возникает в nonterminal response либо caption timestamp выходит
за допустимую границу, результат остаётся ошибочным и требует отдельного разбора.
Настоящий transport disconnect/reconnect не выполнялся. При read-only проверке
текущий Windows token оказался без elevation; точное закрытие отдельного TCP
соединения через [SetTcpEntry](https://learn.microsoft.com/en-us/windows/win32/api/iphlpapi/nf-iphlpapi-settcpentry)
требует административных прав. Глобальная сеть, firewall и TLS interception не
менялись. Fake-provider replay tests не выдаются за эту native проверку.
