# плагин voice — мост Simple Voice Chat + Plasmo Voice (VC1)

Статус: 🚧 реализовано (VC1a/b/c, 2026-10-16). Нативный плагин (уровень **N**,
`plugins/voice`), вкомпилирован в `aether-server` под фичей `plugins`.
Протоколы сверены с исходниками модов:

- **Simple Voice Chat** (henkelmax), ветка **`1.21.11`** (`COMPATIBILITY_VERSION`
  18). Сервер обязан вернуть ту же версию совместимости, что прислал клиент в
  `voicechat:request_secret`, иначе клиент показывает «incompatible version» и
  не подключается.
- **Plasmo Voice** (plasmoapp), v2.x. Плагин-канал `plasmo:voice/v2`.

Серверные части модов переписаны нативно: без Bukkit, Paper и сторонних
серверных плагинов.

## Что работает

- **Оба рукопожатия.** SVC `request_secret → secret`, `update_state`, группы
  (создать/войти/выйти), рассылка состояний; PV `PlayerInfoRequest → PlayerInfo
  → Connection`, затем конфиг и список игроков после первого UDP-пинга. Сервер
  шлёт keepalive'ы, нужные обоим модам (SVC `KeepAlive`, PV `Ping`), и отвечает
  на PV `SourceInfoRequest` пакетом `SourceInfoPacket`.
- **Два UDP-сервера** (opt-in; порты по умолчанию `:24454` SVC, `:60606` PV),
  каждый в своём потоке, декодируют в один внутренний `VoiceFrame`. `AES-GCM`
  (SVC) и `AES-CBC/PKCS5` (PV) реализованы здесь и проверены векторами NIST.
- **Взаимная слышимость.** `frame::route` решает, кто слышит кадр, по близости
  (дистанция кадра, шёпот — вдвое меньше) или по группе; Opus-полезная нагрузка
  **переупаковывается, а не перекодируется**, поэтому говорящий из SVC слышен
  клиенту PV и наоборот — байт в байт.
- **Админ-команды** `/voice join|leave|spy|record` по ноде `aether.voice.admin`
  (операторы проходят всегда). `/voice join` входит в группу без пароля;
  `/voice spy <player|group|all|off>` шлёт копию подходящих кадров голосовому
  клиенту самого админа.
- **Запись** выключена по умолчанию; `/voice record start|stop <player|group|all>`
  пишет по одному Ogg/Opus файлу на говорящего на сессию в `record_dir` с
  метаданными (кто/когда/где/группа) в комментариях `OpusTags`. Отдельный поток
  записи и периодический обход ретеншна держат файловые операции вне тика.

## Конфиг (`[plugins.voice]`)

Оба UDP-листенера — **opt-in**: пока не включите, ничего не биндится.

| ключ | по умолчанию | смысл |
|---|---|---|
| `svc_enabled` / `svc_port` | `false` / `24454` | UDP-сервер Simple Voice Chat |
| `pv_enabled` / `pv_port` | `false` / `60606` | UDP-сервер Plasmo Voice |
| `distance` / `whisper` | `48` / `24` | дистанция слышимости в метрах |
| `groups_enabled` | `true` | предлагать группы SVC |
| `admin_permission` | `aether.voice.admin` | нода, которую проверяет `/voice` |
| `record_dir` | `voice-recordings` | каталог записей |
| `notify_recorded` | `true` | сообщать игроку о записи |
| `retention_days` / `max_disk_mb` | `0` / `0` | `0` — хранить всё |

## Тесты

`cargo-locked test -p aether-voice-plugin` — round-trip протоколов, векторы AES
и MD5, Ogg-писатель с контрольной суммой, сквозной мост, переносящий Opus
SVC→PV и PV→SVC, и серверные тесты, закрепляющие управляющие пакеты (PV
`PlayerInfoRequest`/`Connection`/`Ping`/`SourceInfo`), лимит на отправителя и cap
входящей очереди.

## Ограничения

См. [KNOWN_ISSUES.md §25](../../docs/KNOWN_ISSUES.ru.md). Кратко: группы SVC
живут в памяти и не персистентны; обмен RSA-ключами и сквозное шифрование аудио
PV не реализованы (аудио PV пересылается как есть); транспорт плагин-каналов
подключён для протоколов 764–775 (26.2/26.3 — пока нет); Opus — pass-through,
перекодирование по замыслу не нужно.

