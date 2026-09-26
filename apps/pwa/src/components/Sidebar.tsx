import { Show } from "solid-js";
import { HomeIcon, ChatIcon, FilesIcon, SettingsIcon, SunIcon, MoonIcon, LinkIcon } from "./Icons";
import { connection } from "../stores/connection";
import { t } from "../i18n";

export type Page = "home" | "chat" | "files" | "settings";

interface SidebarProps {
  page: Page;
  setPage: (p: Page) => void;
  theme: string;
  toggleTheme: () => void;
  unread: number;
  drawerOpen: boolean;
  setDrawerOpen: (open: boolean) => void;
}

export default function Sidebar(props: SidebarProps) {
  function navigate(p: Page) {
    props.setPage(p);
    props.setDrawerOpen(false);
  }

  return (
    <>
      {/* Hamburger button — visible only on mobile */}
      <button class="hamburger" onClick={() => props.setDrawerOpen(true)}>
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
          <line x1="3" y1="6" x2="21" y2="6" />
          <line x1="3" y1="12" x2="21" y2="12" />
          <line x1="3" y1="18" x2="21" y2="18" />
        </svg>
      </button>

      {/* Overlay — click to close drawer */}
      <div
        class={`drawer-overlay ${props.drawerOpen ? "open" : ""}`}
        onClick={() => props.setDrawerOpen(false)}
      />

      {/* Sidebar / Drawer */}
      <aside class={`sidebar ${props.drawerOpen ? "open" : ""}`}>
        <div class="sidebar-logo">
          <LinkIcon width="22" height="22" />
          <span>{t().identity_title}</span>
        </div>

        <nav class="sidebar-nav">
          <button
            class={`nav-item ${props.page === "home" ? "active" : ""}`}
            onClick={() => navigate("home")}
          >
            <HomeIcon /> {t().nav_home}
          </button>
          <button
            class={`nav-item ${props.page === "chat" ? "active" : ""}`}
            onClick={() => navigate("chat")}
          >
            <ChatIcon /> {t().nav_chat}
            <Show when={props.unread > 0}>
              <span class="nav-badge">{props.unread}</span>
            </Show>
          </button>
          <button
            class={`nav-item ${props.page === "files" ? "active" : ""}`}
            onClick={() => navigate("files")}
          >
            <FilesIcon /> {t().nav_files}
          </button>
        </nav>

        <div class="sidebar-footer">
          <button
            class={`nav-item ${props.page === "settings" ? "active" : ""}`}
            onClick={() => navigate("settings")}
          >
            <SettingsIcon /> {t().nav_settings}
          </button>
          <button class="nav-item" onClick={props.toggleTheme}>
            <Show when={props.theme === "dark"} fallback={<><MoonIcon /> {t().sidebar_dark_mode}</>}>
              <SunIcon /> {t().sidebar_light_mode}
            </Show>
          </button>
          <div class="sidebar-status">
            <span class={`dot ${connection.connected ? "connected" : ""}`} />
            <span>{connection.connected ? t().status_connected : t().status_offline}</span>
          </div>
        </div>
      </aside>
    </>
  );
}
