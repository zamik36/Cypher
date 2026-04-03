const en = {
  // -- Navigation --
  nav_home: "Home",
  nav_chat: "Chat",
  nav_files: "Files",
  nav_settings: "Settings",

  // -- Status --
  status_connected: "Connected",
  status_disconnected: "Disconnected",
  status_offline: "Offline",
  status_peers: (n: number) => `${n} ${n === 1 ? "peer" : "peers"}`,
  status_active_chats: (n: number) => `${n} active chat${n > 1 ? "s" : ""}`,

  // -- Identity --
  identity_title: "Cypher",
  identity_subtitle: "Anonymous encrypted messenger",
  identity_passphrase: "Passphrase",
  identity_passphrase_min: "Passphrase (min 12 chars)",
  identity_nickname: "Nickname",
  identity_unlock: "Unlock",
  identity_unlocking: "Unlocking...",
  identity_create: "Create identity",
  identity_creating: "Creating...",
  identity_import: "Import",
  identity_importing: "Importing...",
  identity_new: "New identity",
  identity_back_unlock: "Back to unlock",
  identity_back: "Back",
  identity_seed_placeholder: "Seed (64 hex characters)",

  // -- Home --
  home_connecting: "Connecting to network...",
  home_retry: "Retry",
  home_advanced_show: "Advanced options",
  home_advanced_hide: "Hide advanced",
  home_connect: "Connect",
  home_open_chats: "Open Chats",
  home_room_code: "Your Room Code",
  home_copy_code: "Copy Code",
  home_copied: "Copied!",
  home_new_room: "New Room",
  home_waiting: "Waiting for someone to join...",
  home_create_title: "Create Room",
  home_create_desc: "Start a private room and share the code with someone to connect.",
  home_create_btn: "Create Room",
  home_creating: "Creating...",
  home_join_title: "Join Room",
  home_join_desc: "Enter a room code from someone to establish a secure connection.",
  home_join_placeholder: "Enter room code",
  home_join_btn: "Join",
  home_host_port: "host:port",

  // -- Chat --
  chat_empty: "Connect to a peer first to start chatting.",
  chat_go_home: "Go to Home",
  chat_header: "Chats",
  chat_no_messages: "No messages yet",
  chat_select: "Select a chat from the list",
  chat_offline_badge: "offline",
  chat_loading: "Loading history...",
  chat_say_hello: "No messages yet. Say hello!",
  chat_me: "Me",
  chat_peer: "P",
  chat_offline_hint: "Peer is offline. Connect to send messages.",
  chat_placeholder: "Type a message...",

  // -- Files --
  files_title: "Transfers",
  files_empty: "No file transfers yet. Drop a file above or browse to send.",
  files_sending: "Sending",
  files_receiving: "Receiving",
  files_complete: " — Complete",
  files_drop_desktop: "Drag & drop files here, or click to browse",
  files_drop_mobile: "Tap to choose a file",
  files_choose: "Choose File",

  // -- Settings --
  settings_title: "Settings",
  settings_identity: "Identity",
  settings_nickname: "Nickname:",
  settings_peerid: "PeerId:",
  settings_lock: "Lock",
  settings_gateway: "Gateway Server",
  settings_reconnect: "Reconnect",
  settings_reconnecting: "Connecting...",
  settings_theme: "Theme",
  settings_dark: "Dark",
  settings_light: "Light",
  settings_language: "Language",
  settings_notifications: "Notifications",
  settings_notif_blocked: "Blocked by browser — enable in site settings",
  settings_notif_on: "Message notifications are on",
  settings_notif_off: "Message notifications are off",
  settings_notif_enable: "Enable",
  settings_notif_disable: "Disable",
  settings_preview_shown: "Message preview: shown (less private)",
  settings_preview_hidden: "Message preview: hidden (recommended)",
  settings_preview_show: "Show preview",
  settings_preview_hide: "Hide preview",
  settings_export: "Export Seed (Backup)",
  settings_export_placeholder: "Enter passphrase to export",
  settings_export_btn: "Export",
  settings_copy: "Copy",
  settings_data: "Data",
  settings_clear: "Clear chat history",
  settings_clear_warning: "All messages and chat history will be permanently deleted. Ratchet sessions will be reset — reconnecting to peers will require a new key exchange.",
  settings_clear_confirm: (s: number) => s > 0 ? `Confirm (${s}s)` : "Confirm delete",
  settings_cancel: "Cancel",
  settings_about: "About",
  settings_version: "Cypher v0.1.1 (PWA)",
  settings_about_desc: "Anonymous, end-to-end encrypted messaging.",
  settings_about_motto: "No accounts. No tracking. No logs.",

  // -- Toasts --
  toast_peer_connected: "Peer connected!",
  toast_msg_unavailable: "Message history unavailable — messages won't persist across restarts",
  toast_receiving: (name: string) => `Receiving: ${name}`,
  toast_transfer_complete: "Transfer complete!",
  toast_notif_enabled: "Notifications enabled",
  toast_notif_denied: "Notification permission denied by browser",
  toast_notif_disabled: "Notifications disabled",
  toast_history_cleared: "Chat history cleared",
  toast_clear_failed: (e: string) => `Failed to clear: ${e}`,
  toast_seed_copied: "Seed copied!",

  // -- Install prompt --
  install_text: "Install Cypher for the best experience",
  install_btn: "Install",
  install_ios: "Share, then \"Add to Home Screen\"",
  install_android: "Menu \u2630 then \"Add to Home Screen\" or \"Install app\"",

  // -- Sidebar --
  sidebar_light_mode: "Light mode",
  sidebar_dark_mode: "Dark mode",
};

export type TranslationKeys = {
  [K in keyof typeof en]: (typeof en)[K] extends (...args: infer A) => string
    ? (...args: A) => string
    : string;
};

export default en as TranslationKeys;
