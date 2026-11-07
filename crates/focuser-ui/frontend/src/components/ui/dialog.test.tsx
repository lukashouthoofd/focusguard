import { fireEvent, render, screen, within } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { CommandError } from "@/lib/transport";
import { Dialog, ErrorDialog } from "./dialog";

it("renders nothing while it is closed", () => {
  render(
    <Dialog open={false} title="Lock videos" onClose={vi.fn()}>
      <p>Body</p>
    </Dialog>,
  );

  expect(screen.queryByText("Body")).toBeNull();
});

it("opens as a modal named by its title", () => {
  render(
    <Dialog open title="Lock videos" onClose={vi.fn()}>
      <p>Body</p>
    </Dialog>,
  );

  const dialog = screen.getByRole("dialog", { name: "Lock videos" });
  expect(dialog).toHaveAttribute("open");
  expect(within(dialog).getByText("Body")).toBeVisible();
});

it("closes from its button, from Escape and from a click outside", () => {
  const onClose = vi.fn();
  render(
    <Dialog open title="Lock videos" onClose={onClose}>
      <p>Body</p>
    </Dialog>,
  );
  const dialog = screen.getByRole("dialog");

  fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
  expect(onClose).toHaveBeenCalledTimes(1);

  // Escape reaches a modal dialog as a `cancel` event.
  fireEvent(dialog, new Event("cancel", { cancelable: true }));
  expect(onClose).toHaveBeenCalledTimes(2);

  // A click on the backdrop is a click on the dialog element itself.
  fireEvent.click(dialog);
  expect(onClose).toHaveBeenCalledTimes(3);
});

it("stays open for a click inside it", () => {
  const onClose = vi.fn();
  render(
    <Dialog open title="Lock videos" onClose={onClose}>
      <p>Body</p>
    </Dialog>,
  );

  fireEvent.click(screen.getByText("Body"));
  fireEvent.click(screen.getByText("Lock videos"));

  expect(onClose).not.toHaveBeenCalled();
});

it("gives the focus back to where it was when it closes", () => {
  function Host({ open }: { open: boolean }) {
    return (
      <>
        <button type="button">opener</button>
        <Dialog open={open} title="Lock videos" onClose={vi.fn()}>
          <p>Body</p>
        </Dialog>
      </>
    );
  }
  const { rerender } = render(<Host open={false} />);
  const opener = screen.getByRole("button", { name: "opener" });
  opener.focus();

  rerender(<Host open />);
  rerender(<Host open={false} />);

  expect(opener).toHaveFocus();
});

it("still shows what is in it where the browser has no modal dialogs", () => {
  // WebKit before 15.4, which is what macOS before 12.3 gives the app.
  const showModal = HTMLDialogElement.prototype.showModal;
  Reflect.deleteProperty(HTMLDialogElement.prototype, "showModal");
  try {
    render(
      <Dialog open title="Lock videos" onClose={vi.fn()}>
        <p>Body</p>
      </Dialog>,
    );

    expect(screen.getByText("Body")).toBeVisible();
  } finally {
    HTMLDialogElement.prototype.showModal = showModal;
  }
});

it("puts the focus on the part that asks for it", () => {
  render(
    <Dialog open title="Unlock videos" onClose={vi.fn()}>
      <input aria-label="answer" data-autofocus />
    </Dialog>,
  );

  expect(screen.getByLabelText("answer")).toHaveFocus();
});

it("shows a failed action as a message with one button", () => {
  const onClose = vi.fn();
  const refused = new CommandError({ code: "protected", message: "list is protected" });
  render(<ErrorDialog error={refused} onClose={onClose} />);

  const dialog = screen.getByRole("alertdialog", { name: "Something went wrong" });
  // The code is what gets translated, not the Rust prose.
  expect(
    within(dialog).getByText(
      "A lock is running on this list, so it cannot be changed until the lock expires.",
    ),
  ).toBeVisible();

  fireEvent.click(within(dialog).getByRole("button", { name: "OK" }));
  expect(onClose).toHaveBeenCalledTimes(1);
});

it("puts the focus on OK, so Enter is enough to dismiss a message", () => {
  render(<ErrorDialog error={new Error("no")} onClose={vi.fn()} />);

  expect(screen.getByRole("button", { name: "OK" })).toHaveFocus();
});

it("shows no message while there is no error", () => {
  render(<ErrorDialog error={null} onClose={vi.fn()} />);

  expect(screen.queryByRole("alertdialog")).toBeNull();
});
