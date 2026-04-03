import type { TranslationKeys } from "./en";

const ru: TranslationKeys = {
  // -- Navigation --
  nav_home: "Главная",
  nav_chat: "Чат",
  nav_files: "Файлы",
  nav_settings: "Настройки",

  // -- Status --
  status_connected: "Подключен",
  status_disconnected: "Отключен",
  status_offline: "Не в сети",
  status_peers: (n: number) => {
    if (n % 10 === 1 && n % 100 !== 11) return `${n} пир`;
    if (n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 10 || n % 100 >= 20)) return `${n} пира`;
    return `${n} пиров`;
  },
  status_active_chats: (n: number) => {
    if (n === 1) return `${n} активный чат`;
    if (n >= 2 && n <= 4) return `${n} активных чата`;
    return `${n} активных чатов`;
  },

  // -- Identity --
  identity_title: "Шифр",
  identity_subtitle: "Анонимный зашифрованный мессенджер",
  identity_passphrase: "Пароль",
  identity_passphrase_min: "Пароль (мин. 12 символов)",
  identity_nickname: "Никнейм",
  identity_unlock: "Разблокировать",
  identity_unlocking: "Разблокировка...",
  identity_create: "Создать личность",
  identity_creating: "Создание...",
  identity_import: "Импорт",
  identity_importing: "Импорт...",
  identity_new: "Новая личность",
  identity_back_unlock: "Назад к разблокировке",
  identity_back: "Назад",
  identity_seed_placeholder: "Seed (64 hex символа)",

  // -- Home --
  home_connecting: "Подключение к сети...",
  home_retry: "Повторить",
  home_advanced_show: "Дополнительно",
  home_advanced_hide: "Скрыть",
  home_connect: "Подключить",
  home_open_chats: "Открыть чаты",
  home_room_code: "Код комнаты",
  home_copy_code: "Копировать",
  home_copied: "Скопировано!",
  home_new_room: "Новая комната",
  home_waiting: "Ожидание подключения...",
  home_create_title: "Создать комнату",
  home_create_desc: "Создайте приватную комнату и поделитесь кодом для подключения.",
  home_create_btn: "Создать",
  home_creating: "Создание...",
  home_join_title: "Войти в комнату",
  home_join_desc: "Введите код комнаты для установки защищённого соединения.",
  home_join_placeholder: "Введите код комнаты",
  home_join_btn: "Войти",
  home_host_port: "хост:порт",

  // -- Chat --
  chat_empty: "Сначала подключитесь к собеседнику.",
  chat_go_home: "На главную",
  chat_header: "Чаты",
  chat_no_messages: "Нет сообщений",
  chat_select: "Выберите чат из списка",
  chat_offline_badge: "не в сети",
  chat_loading: "Загрузка истории...",
  chat_say_hello: "Сообщений пока нет. Скажите привет!",
  chat_me: "Я",
  chat_peer: "С",
  chat_offline_hint: "Собеседник не в сети. Подключитесь для отправки.",
  chat_placeholder: "Введите сообщение...",

  // -- Files --
  files_title: "Передачи",
  files_empty: "Нет передач файлов. Перетащите файл или выберите для отправки.",
  files_sending: "Отправка",
  files_receiving: "Получение",
  files_complete: " — Завершено",
  files_drop_desktop: "Перетащите файлы сюда или нажмите для выбора",
  files_drop_mobile: "Нажмите для выбора файла",
  files_choose: "Выбрать файл",

  // -- Settings --
  settings_title: "Настройки",
  settings_identity: "Личность",
  settings_nickname: "Никнейм:",
  settings_peerid: "PeerId:",
  settings_lock: "Заблокировать",
  settings_gateway: "Сервер шлюза",
  settings_reconnect: "Переподключить",
  settings_reconnecting: "Подключение...",
  settings_theme: "Тема",
  settings_dark: "Тёмная",
  settings_light: "Светлая",
  settings_language: "Язык",
  settings_notifications: "Уведомления",
  settings_notif_blocked: "Заблокировано браузером — включите в настройках сайта",
  settings_notif_on: "Уведомления о сообщениях включены",
  settings_notif_off: "Уведомления о сообщениях выключены",
  settings_notif_enable: "Включить",
  settings_notif_disable: "Выключить",
  settings_preview_shown: "Предпросмотр: показан (менее приватно)",
  settings_preview_hidden: "Предпросмотр: скрыт (рекомендуется)",
  settings_preview_show: "Показать",
  settings_preview_hide: "Скрыть",
  settings_export: "Экспорт Seed (Резервная копия)",
  settings_export_placeholder: "Введите пароль для экспорта",
  settings_export_btn: "Экспорт",
  settings_copy: "Копировать",
  settings_data: "Данные",
  settings_clear: "Очистить историю",
  settings_clear_warning: "Все сообщения и история чатов будут удалены безвозвратно. Сессии шифрования будут сброшены — для переподключения потребуется новый обмен ключами.",
  settings_clear_confirm: (s: number) => s > 0 ? `Подтвердить (${s}с)` : "Подтвердить удаление",
  settings_cancel: "Отмена",
  settings_about: "О приложении",
  settings_version: "Шифр v0.1.1 (PWA)",
  settings_about_desc: "Анонимный мессенджер со сквозным шифрованием.",
  settings_about_motto: "Без аккаунтов. Без слежки. Без логов.",

  // -- Toasts --
  toast_peer_connected: "Собеседник подключён!",
  toast_msg_unavailable: "История сообщений недоступна — сообщения не сохранятся между перезагрузками",
  toast_receiving: (name: string) => `Получение: ${name}`,
  toast_transfer_complete: "Передача завершена!",
  toast_notif_enabled: "Уведомления включены",
  toast_notif_denied: "Разрешение на уведомления отклонено браузером",
  toast_notif_disabled: "Уведомления выключены",
  toast_history_cleared: "История чатов очищена",
  toast_clear_failed: (e: string) => `Ошибка очистки: ${e}`,
  toast_seed_copied: "Seed скопирован!",

  // -- Install prompt --
  install_text: "Установите Шифр для лучшего опыта",
  install_btn: "Установить",
  install_ios: "Поделиться, затем \"На экран Домой\"",
  install_android: "Меню \u2630, затем \"На экран Домой\" или \"Установить\"",

  // -- Sidebar --
  sidebar_light_mode: "Светлая тема",
  sidebar_dark_mode: "Тёмная тема",
};

export default ru;
