import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { changeAppLanguage } from "../i18n";
import { PageFindBar, runBrowserPageFind } from "./PageFindBar";

describe("PageFindBar", () => {
  test("opens with Command-F and searches forward or backward", async () => {
    await changeAppLanguage("en-US");
    const findText = vi.fn(() => true);
    const user = userEvent.setup();
    render(<PageFindBar enabled findText={findText} />);

    await user.keyboard("{Meta>}f{/Meta}");
    const input = await screen.findByRole("searchbox", { name: "Search this page" });
    await user.type(input, "model");
    expect(findText).toHaveBeenLastCalledWith("model", { backwards: false, restart: true });

    await user.keyboard("{Enter}");
    expect(findText).toHaveBeenLastCalledWith("model", { backwards: false, restart: false });
    await user.keyboard("{Shift>}{Enter}{/Shift}");
    expect(findText).toHaveBeenLastCalledWith("model", { backwards: true, restart: false });

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("searchbox", { name: "Search this page" })).not.toBeInTheDocument();
  });

  test("reports a missing match without installing the bar on other platforms", async () => {
    await changeAppLanguage("en-US");
    const user = userEvent.setup();
    const view = render(<PageFindBar enabled={false} findText={() => false} />);
    await user.keyboard("{Meta>}f{/Meta}");
    expect(screen.queryByRole("searchbox")).not.toBeInTheDocument();

    view.rerender(<PageFindBar enabled findText={() => false} />);
    await user.keyboard("{Meta>}f{/Meta}");
    await user.type(screen.getByRole("searchbox"), "absent");
    expect(screen.getByRole("status")).toHaveTextContent("No matches");
  });
});

test("browser find restarts from the beginning and wraps", () => {
  const find = vi.fn(() => true);
  const selection = {
    removeAllRanges: vi.fn(),
    addRange: vi.fn(),
  };
  const range = {
    selectNodeContents: vi.fn(),
    collapse: vi.fn(),
  };
  const browserWindow = {
    find,
    getSelection: () => selection,
  } as unknown as Window & { find: typeof find };
  const browserDocument = {
    body: {},
    createRange: () => range,
  } as unknown as Document;

  expect(runBrowserPageFind(
    "model",
    { backwards: false, restart: true },
    browserWindow,
    browserDocument,
  )).toBe(true);
  expect(range.collapse).toHaveBeenCalledWith(true);
  expect(find).toHaveBeenCalledWith("model", false, false, true, false, true, false);
});
