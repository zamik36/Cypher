import { Match, Show, Switch } from "solid-js";
import "./AppShell.css";
import ChatList from "./ChatList";
import ChatScreen from "./ChatScreen";
import NewChat from "./NewChat";
import ContactScreen from "./ContactScreen";
import Settings from "./Settings";
import ConnectionBanner from "../components/ConnectionBanner";
import ToastContainer from "../components/ToastContainer";
import Icon from "../components/Icon";
import { isWide } from "../stores/layout";
import { top, type Screen } from "../stores/nav";
import { t } from "../i18n";

/** The parameter of the top screen when it is `name`, for keyed rendering. */
function param<N extends Screen["name"]>(name: N): Extract<Screen, { name: N }> | undefined {
  const screen = top();
  return screen.name === name ? (screen as Extract<Screen, { name: N }>) : undefined;
}

/**
 * One screen at a time on a phone; on a wide window the chat list stays on
 * the left and the open screen fills the right.
 */
export default function AppShell() {
  const atRoot = () => top().name === "chats";
  return (
    <div class="shell" classList={{ "shell--wide": isWide() }}>
      <ConnectionBanner />
      <div class="shell__panes">
        <Show when={isWide() || atRoot()}>
          <div class="shell__list">
            <ChatList />
          </div>
        </Show>
        <Show when={isWide() || !atRoot()}>
          <main class="shell__main">
            <Switch
              fallback={
                <div class="empty">
                  <span class="empty__icon">
                    <Icon name="message-circle" size={28} />
                  </span>
                  <p>{t().chats_select}</p>
                </div>
              }
            >
              <Match when={param("chat")?.peerId} keyed>
                {(peerId) => <ChatScreen peerId={peerId} />}
              </Match>
              <Match when={param("new-chat")} keyed>
                {(screen) => <NewChat tab={screen.tab} />}
              </Match>
              <Match when={param("contact")?.peerId} keyed>
                {(peerId) => <ContactScreen peerId={peerId} />}
              </Match>
              <Match when={param("settings")}>
                <Settings />
              </Match>
              <Match when={param("settings-section")?.section} keyed>
                {(section) => <Settings section={section} />}
              </Match>
            </Switch>
          </main>
        </Show>
      </div>
      <ToastContainer />
    </div>
  );
}
