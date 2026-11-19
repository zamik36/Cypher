import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import ActionMenu, { type MenuAction } from "./ActionMenu";

function setup() {
  const anchor = document.createElement("button");
  anchor.textContent = "anchor";
  document.body.append(anchor);
  const copy = vi.fn();
  const remove = vi.fn();
  const onClose = vi.fn();
  const actions: MenuAction[] = [
    { label: "Copy", icon: "copy", run: copy },
    { label: "Delete", icon: "trash", danger: true, run: remove },
  ];
  const view = render(() => (
    <ActionMenu anchor={anchor} align="end" actions={actions} label="Message actions" onClose={onClose} />
  ));
  return { anchor, copy, remove, onClose, view };
}

describe("ActionMenu", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it("offers its actions, focuses the first, and runs a choice after closing", () => {
    const { copy, remove, onClose } = setup();
    const menu = screen.getByRole("menu", { name: "Message actions" });
    const items = screen.getAllByRole("menuitem");
    expect(items.map((i) => i.textContent)).toEqual(["Copy", "Delete"]);
    expect(document.activeElement).toBe(items[0]);
    expect(items[1]?.classList.contains("action-menu__item--danger")).toBe(true);
    expect(menu.classList.contains("action-menu--end")).toBe(true);

    fireEvent.click(screen.getByRole("menuitem", { name: "Delete" }));
    expect(onClose).toHaveBeenCalledOnce();
    expect(remove).toHaveBeenCalledOnce();
    expect(copy).not.toHaveBeenCalled();
  });

  it("closes on Escape without letting the screen go back, and returns focus", () => {
    const { anchor, onClose } = setup();
    const screenKeys = vi.fn();
    window.addEventListener("keydown", screenKeys);
    fireEvent.keyDown(window, { key: "Enter" });
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
    expect(document.activeElement).toBe(anchor);
    expect(screenKeys).toHaveBeenCalledOnce();
    window.removeEventListener("keydown", screenKeys);
  });

  it("closes on a tap elsewhere, a scroll or a resize, not on a tap inside", () => {
    const { anchor, onClose, view } = setup();
    fireEvent.pointerDown(screen.getByRole("menuitem", { name: "Copy" }));
    fireEvent.pointerDown(anchor);
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.pointerDown(document.body);
    fireEvent.scroll(document);
    fireEvent(window, new Event("resize"));
    expect(onClose).toHaveBeenCalledTimes(3);

    view.unmount();
    fireEvent.pointerDown(document.body);
    expect(onClose).toHaveBeenCalledTimes(3);
  });
});
