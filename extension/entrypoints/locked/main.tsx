import { i18n } from "#i18n";
import { Lock } from "lucide-react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@/assets/tailwind.css";

/**
 * Where the browser's extensions page goes while a block list is locked.
 * There the extension could be switched off, so the background worker sends
 * the tab here instead.
 */
function Locked() {
  return (
    <main className="flex min-h-screen w-full flex-col items-center justify-center bg-background px-6 text-center">
      <div className="flex size-14 items-center justify-center rounded-2xl bg-primary/15 text-primary">
        <Lock className="size-7" />
      </div>
      <h1 className="mt-6 font-semibold text-3xl text-foreground">{i18n.t("locked.title")}</h1>
      <p className="mt-3 max-w-md text-pretty text-base text-muted-foreground leading-relaxed">
        {i18n.t("locked.body")}
      </p>
    </main>
  );
}

const root = document.getElementById("root");
if (root) {
  document.title = i18n.t("locked.title");
  createRoot(root).render(
    <StrictMode>
      <Locked />
    </StrictMode>,
  );
}
