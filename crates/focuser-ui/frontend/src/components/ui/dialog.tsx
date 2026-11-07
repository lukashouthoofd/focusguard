import { AlertTriangle, X } from "lucide-react";
import { type ReactNode, useEffect, useId, useRef, useState } from "react";
import { errorMessage } from "@/lib/errors";
import { cn } from "@/lib/utils";
import { m } from "@/paraglide/messages.js";
import { Button } from "./button";

interface DialogProps {
  open: boolean;
  /** Escape, the close button and a click outside all end up here. */
  onClose: () => void;
  title: string;
  /** Shown before the title. */
  icon?: ReactNode;
  children: ReactNode;
  /** `alertdialog` for a message that interrupts, the default for a task. */
  role?: "dialog" | "alertdialog";
  className?: string;
}

/**
 * A pop-up for one task or one message.
 *
 * This is the native `<dialog>`: the browser traps focus, makes the page behind
 * it inert and answers Escape, none of which is worth rebuilding.
 *
 * One thing comes with that. A modal dialog sits in the top layer, above
 * everything that is not inside it, so the Radix `Select` and `Tooltip`, which
 * render into `<body>`, would open *behind* it. Keep them out of a dialog.
 *
 * The browser puts the focus on the first control, which is the close button.
 * Mark the part the pop-up is for with `data-autofocus` to start there instead.
 */
export function Dialog({ open, ...rest }: DialogProps) {
  // Mounted only while open, so opening is always a fresh `showModal()` and a
  // closed dialog leaves nothing in the page for a test or a reader to find.
  return open ? <OpenDialog {...rest} /> : null;
}

function OpenDialog({ onClose, title, icon, children, role, className }: Omit<DialogProps, "open">) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  // Read while rendering, not in the effect. In development React runs an
  // effect twice, and by the second run the focus is already inside the dialog.
  const [opener] = useState(() => document.activeElement);

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    // WebKit before 15.4, which macOS before 12.3 gives the app, has no modal
    // dialogs. Opened in place the form inside still works, and that beats an
    // error that takes the whole window down.
    if (typeof dialog.showModal !== "function") dialog.setAttribute("open", "");
    else if (!dialog.open) dialog.showModal();
    dialog.querySelector<HTMLElement>("[data-autofocus]")?.focus();
    // `close()` hands the focus back; taking the element out of the page does
    // not, and this one is closed by unmounting.
    return () => {
      if (opener instanceof HTMLElement) opener.focus();
    };
  }, [opener]);

  return (
    <dialog
      ref={ref}
      role={role}
      aria-labelledby={titleId}
      // Escape. Prevented so the parent's state stays the one thing that
      // decides whether this is open.
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      // The panel below fills the element, so a click that lands on the element
      // itself landed on the backdrop.
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
      // Preflight zeroes the margin that centres a dialog; `m-auto` puts it back.
      className={cn(
        "m-auto w-[min(28rem,calc(100vw-2rem))] rounded-2xl border border-border-strong p-0",
        "bg-elevated text-foreground shadow-(--shadow-depth-lg)",
        "backdrop:bg-black/60 backdrop:backdrop-blur-sm",
        "animate-in fade-in zoom-in-95",
        className,
      )}
    >
      <div className="p-5">
        <div className="flex items-start gap-2.5">
          {icon && <span className="mt-0.5 shrink-0 [&_svg]:size-4.5">{icon}</span>}
          <h2 id={titleId} className="min-w-0 flex-1 font-medium text-base text-foreground">
            {title}
          </h2>
          <Button
            variant="ghost"
            size="icon"
            className="-mt-1.5 -mr-2 size-8"
            aria-label={m.common_close()}
            onClick={onClose}
          >
            <X />
          </Button>
        </div>
        {children}
      </div>
    </dialog>
  );
}

/**
 * A failed action, as something to read and dismiss.
 *
 * It replaces a red line under the control. A line like that stays on the page
 * after it has been read, and it sits wherever the layout had room, which is
 * rarely where the eye is.
 */
export function ErrorDialog({ error, onClose }: { error: Error | null; onClose: () => void }) {
  return (
    <Dialog
      open={error !== null}
      onClose={onClose}
      role="alertdialog"
      title={m.common_error()}
      icon={<AlertTriangle aria-hidden className="text-warning" />}
    >
      <p className="mt-2 text-muted-foreground text-sm leading-relaxed">
        {error ? errorMessage(error) : null}
      </p>
      <div className="mt-5 flex justify-end">
        <Button variant="outline" size="sm" data-autofocus onClick={onClose}>
          {m.common_ok()}
        </Button>
      </div>
    </Dialog>
  );
}
