import { createSignal, For, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import "./ActionMenu.css";
import Icon, { type IconName } from "./Icon";

export interface MenuAction {
  label: string;
  icon: IconName;
  danger?: boolean;
  run: () => void;
}

const GAP = 6;
const MARGIN = 8;

/**
 * Actions for what was tapped, in a small menu next to it: below when there
 * is room, else above; aligned to the anchor's `align` edge. Closes on a
 * choice, a tap elsewhere, Escape, scrolling or resizing.
 */
export default function ActionMenu(props: {
  anchor: HTMLElement;
  align: "start" | "end";
  actions: readonly MenuAction[];
  label: string;
  onClose: () => void;
}) {
  let menu: HTMLDivElement | undefined;
  const [place, setPlace] = createSignal({ top: 0, left: 0, up: false });

  function position() {
    if (!menu) return;
    const anchor = props.anchor.getBoundingClientRect();
    const { width, height } = menu.getBoundingClientRect();
    const up = anchor.bottom + GAP + height > window.innerHeight - MARGIN && anchor.top - GAP - height > MARGIN;
    const wanted = props.align === "end" ? anchor.right - width : anchor.left;
    setPlace({
      top: up ? anchor.top - GAP - height : anchor.bottom + GAP,
      left: Math.min(Math.max(wanted, MARGIN), window.innerWidth - width - MARGIN),
      up,
    });
  }

  onMount(() => {
    position();
    menu?.querySelector<HTMLButtonElement>("[role=menuitem]")?.focus();
    const away = (e: PointerEvent) => {
      const target = e.target as Node;
      if (!menu?.contains(target) && !props.anchor.contains(target)) props.onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Ours, not the screen's: Escape would otherwise also go back.
      e.preventDefault();
      e.stopPropagation();
      props.onClose();
      props.anchor.focus();
    };
    const close = () => props.onClose();
    document.addEventListener("pointerdown", away, true);
    window.addEventListener("keydown", key, true);
    window.addEventListener("resize", close);
    document.addEventListener("scroll", close, true);
    onCleanup(() => {
      document.removeEventListener("pointerdown", away, true);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", close);
      document.removeEventListener("scroll", close, true);
    });
  });

  return (
    <Portal>
      <div
        ref={menu}
        class="action-menu"
        classList={{ "action-menu--up": place().up, "action-menu--end": props.align === "end" }}
        style={{ top: `${place().top}px`, left: `${place().left}px` }}
        role="menu"
        aria-label={props.label}
      >
        <For each={props.actions}>
          {(action) => (
            <button
              class="action-menu__item"
              classList={{ "action-menu__item--danger": action.danger === true }}
              role="menuitem"
              onClick={() => {
                props.onClose();
                action.run();
              }}
            >
              <Icon name={action.icon} size={18} />
              <span>{action.label}</span>
            </button>
          )}
        </For>
      </div>
    </Portal>
  );
}
