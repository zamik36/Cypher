import { Show } from "solid-js";
import { HomeIcon, ChatIcon, FilesIcon, SettingsIcon } from "./Icons";
export default function BottomNav(props) {
    return (<nav class="bottom-nav">
      <button class={`bottom-nav-item ${props.page === "home" ? "active" : ""}`} onClick={() => props.setPage("home")}>
        <HomeIcon width="22" height="22"/>
        <span>Home</span>
      </button>
      <button class={`bottom-nav-item ${props.page === "chat" ? "active" : ""}`} onClick={() => props.setPage("chat")}>
        <span class="bottom-nav-icon-wrap">
          <ChatIcon width="22" height="22"/>
          <Show when={props.unread > 0}>
            <span class="bottom-nav-badge">{props.unread > 99 ? "99+" : props.unread}</span>
          </Show>
        </span>
        <span>Chat</span>
      </button>
      <button class={`bottom-nav-item ${props.page === "files" ? "active" : ""}`} onClick={() => props.setPage("files")}>
        <FilesIcon width="22" height="22"/>
        <span>Files</span>
      </button>
      <button class={`bottom-nav-item ${props.page === "settings" ? "active" : ""}`} onClick={() => props.setPage("settings")}>
        <SettingsIcon width="22" height="22"/>
        <span>Settings</span>
      </button>
    </nav>);
}
