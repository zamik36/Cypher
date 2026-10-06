import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import FileCard from "./FileCard";
import { registerPlatform, type Platform, type UiFile } from "../platform";
import { transferOf, upsertTransfer } from "../stores/transfers";

const calls = {
  acceptFile: vi.fn<(id: string) => Promise<void>>().mockResolvedValue(undefined),
  cancelTransfer: vi.fn<(id: string) => Promise<void>>().mockResolvedValue(undefined),
  openFile: vi.fn<(file: UiFile) => Promise<void>>().mockResolvedValue(undefined),
  revealFile: vi.fn<(id: string) => Promise<void>>().mockResolvedValue(undefined),
};

/** A platform where `saved` files are on this device and pictures have a URL. */
function platform(kind: "desktop" | "web", saved: readonly string[]) {
  registerPlatform({
    kind,
    capabilities: { tor: false, gatewayConfig: false, revealFile: kind === "desktop", streamMedia: false },
    fileSaved: (id: string) => Promise.resolve(saved.includes(id)),
    imageUrl: (id: string) => Promise.resolve(`blob:picture-${id}`),
    ...calls,
  } as unknown as Platform);
}

let next = 0;
function file(name: string, mime = ""): UiFile {
  next += 1;
  return { file_id: `f${next}`, name, size: 2048, mime, kind: "file", duration_ms: null };
}

const menuItems = () => screen.getAllByRole("menuitem").map((item) => item.textContent);

describe("FileCard", () => {
  beforeEach(() => {
    for (const call of Object.values(calls)) call.mockClear();
  });
  afterEach(cleanup);

  it("offers an incoming file: download it, then follow the transfer", async () => {
    platform("desktop", []);
    const offer = file("report.pdf");
    upsertTransfer({ file_id: offer.file_id, status: "offered" });
    render(() => <FileCard file={offer} outgoing={false} />);
    const card = screen.getByRole("button", { name: "report.pdf" });
    expect(card.textContent).toContain("2 KB · download");

    fireEvent.click(card);
    expect(menuItems()).toEqual(["Download", "Decline"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Download" }));
    expect(calls.acceptFile).toHaveBeenCalledWith(offer.file_id);
    await waitFor(() => expect(transferOf(offer.file_id)?.status).toBe("active"));

    upsertTransfer({ file_id: offer.file_id, progress: 0.42 });
    await waitFor(() => expect(card.textContent).toContain("42%"));
    fireEvent.click(card);
    fireEvent.click(screen.getByRole("menuitem", { name: "Cancel download" }));
    await waitFor(() => expect(card.textContent).toContain("Cancelled"));
    expect(calls.cancelTransfer).toHaveBeenCalledWith(offer.file_id);
    expect(card).toHaveProperty("disabled", true);
  });

  it("declines an offer", async () => {
    platform("desktop", []);
    const offer = file("spam.zip");
    upsertTransfer({ file_id: offer.file_id, status: "offered" });
    render(() => <FileCard file={offer} outgoing={false} />);
    fireEvent.click(screen.getByRole("button", { name: "spam.zip" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Decline" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "spam.zip" }).textContent).toContain("Declined"));
  });

  it("shows a kept picture, opens it full screen, in its app and in its folder", async () => {
    const picture = file("sunset.png", "image/png");
    platform("desktop", [picture.file_id]);
    render(() => <FileCard file={picture} outgoing />);
    const card = screen.getByRole("button", { name: "sunset.png" });
    await waitFor(() => expect(card.querySelector("img")?.getAttribute("src")).toBe(`blob:picture-${picture.file_id}`));
    expect(card.textContent).toContain("Sent");

    fireEvent.click(card);
    expect(menuItems()).toEqual(["View", "Open file", "Show in folder"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Show in folder" }));
    expect(calls.revealFile).toHaveBeenCalledWith(picture.file_id);
    fireEvent.click(card);
    fireEvent.click(screen.getByRole("menuitem", { name: "Open file" }));
    expect(calls.openFile).toHaveBeenCalledWith(picture);

    fireEvent.click(card);
    fireEvent.click(screen.getByRole("menuitem", { name: "View" }));
    const viewer = screen.getByRole("dialog", { name: "sunset.png" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close" }));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(viewer.isConnected).toBe(false);

    // A picture the browser cannot show falls back to the icon.
    const img = card.querySelector("img");
    if (!img) throw new Error("the preview is gone");
    fireEvent.error(img);
    await waitFor(() => expect(card.querySelector("img")).toBeNull());
  });

  it("saves rather than opens in a browser, with no folder to show", async () => {
    const notes = file("notes.txt");
    platform("web", [notes.file_id]);
    render(() => <FileCard file={notes} outgoing={false} />);
    const card = screen.getByRole("button", { name: "notes.txt" });
    await waitFor(() => expect(card.textContent).toContain("Saved"));
    fireEvent.click(card);
    expect(menuItems()).toEqual(["Save"]);
  });

  it("has nothing to offer for a file that is gone or failed", async () => {
    platform("desktop", []);
    const gone = file("old.doc");
    const failed = file("broken.bin");
    upsertTransfer({ file_id: failed.file_id, status: "error", error: "Peer went away" });
    render(() => (
      <>
        <FileCard file={gone} outgoing={false} />
        <FileCard file={failed} outgoing />
      </>
    ));
    await waitFor(() => expect(screen.getByRole("button", { name: "old.doc" })).toHaveProperty("disabled", true));
    const broken = screen.getByRole("button", { name: "broken.bin" });
    expect(broken.textContent).toContain("Peer went away");
    expect(broken).toHaveProperty("disabled", true);
  });
});
