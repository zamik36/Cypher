import { Show, type JSX } from "solid-js";
import Icon from "./Icon";
import { t } from "../i18n";

/** A screen's header: optional back button, title (or custom content), actions. */
export default function TopBar(props: {
  title?: string;
  subtitle?: string;
  onBack?: (() => void) | undefined;
  children?: JSX.Element;
  actions?: JSX.Element;
  testId?: string;
}) {
  return (
    <header class="topbar" data-testid={props.testId}>
      <Show when={props.onBack}>
        {(back) => (
          <button class="icon-btn" onClick={() => back()()} aria-label={t().common_back}>
            <Icon name="arrow-left" />
          </button>
        )}
      </Show>
      <div class="topbar__title">
        <Show when={props.children} fallback={<h1>{props.title}</h1>}>
          {props.children}
        </Show>
        <Show when={props.subtitle}>
          <span class="topbar__subtitle">{props.subtitle}</span>
        </Show>
      </div>
      {props.actions}
    </header>
  );
}
