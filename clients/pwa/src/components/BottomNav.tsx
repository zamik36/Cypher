import { Show } from "solid-js";
import { HomeIcon, ChatIcon, FilesIcon, SettingsIcon } from "./Icons";
import type { Page } from "./Sidebar";
import { t } from "../i18n";

interface BottomNavProps {
  page: Page;
  setPage: (p: Page) => void;
  unread: number;
}

export default function BottomNav(props: BottomNavProps) {
  return (
    <nav class="bottom-nav">
      <button
        class={`bottom-nav-item ${props.page === "home" ? "active" : ""}`}
        onClick={() => props.setPage("home")}
      >
        <HomeIcon width="22" height="22" />
        <span>{t().nav_home}</span>
      </button>
      <button
        class={`bottom-nav-item ${props.page === "chat" ? "active" : ""}`}
        onClick={() => props.setPage("chat")}
      >
        <span class="bottom-nav-icon-wrap">
          <ChatIcon width="22" height="22" />
          <Show when={props.unread > 0}>
            <span class="bottom-nav-badge">{props.unread > 99 ? "99+" : props.unread}</span>
          </Show>
        </span>
        <span>{t().nav_chat}</span>
      </button>
      <button
        class={`bottom-nav-item ${props.page === "files" ? "active" : ""}`}
        onClick={() => props.setPage("files")}
      >
        <FilesIcon width="22" height="22" />
        <span>{t().nav_files}</span>
      </button>
      <button
        class={`bottom-nav-item ${props.page === "settings" ? "active" : ""}`}
        onClick={() => props.setPage("settings")}
      >
        <SettingsIcon width="22" height="22" />
        <span>{t().nav_settings}</span>
      </button>
    </nav>
  );
}
