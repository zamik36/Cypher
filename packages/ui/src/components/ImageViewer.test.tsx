import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import ImageViewer from "./ImageViewer";

describe("ImageViewer", () => {
  afterEach(cleanup);

  it("closes on a tap, the close button or Escape, and on nothing else", () => {
    const onClose = vi.fn();
    render(() => <ImageViewer src="blob:sunset" alt="sunset.png" onClose={onClose} />);
    const viewer = screen.getByRole("dialog", { name: "sunset.png" });
    expect(screen.getByRole("img", { name: "sunset.png" }).getAttribute("src")).toBe("blob:sunset");

    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Escape" });
    fireEvent.click(viewer);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(3);
  });
});
