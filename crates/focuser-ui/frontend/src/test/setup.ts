import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/react";
import { beforeEach } from "vitest";

// The default second is not enough for a first render on a busy CI runner,
// and a `findBy` that times out there fails a test that is otherwise right.
configure({ asyncUtilTimeout: 5000 });

// jsdom has the <dialog> element but none of its methods. This is the least
// that lets a component open one: the `open` attribute, and a `close` event.
if (typeof HTMLDialogElement !== "undefined" && !HTMLDialogElement.prototype.showModal) {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function close() {
    this.removeAttribute("open");
    this.dispatchEvent(new Event("close"));
  };
}

/**
 * A `localStorage` for tests.
 *
 * Paraglide resolves the locale from it on every message call, so without one
 * every render throws before it draws anything. jsdom does not reliably expose
 * it here, and a Map is enough for what we ask of it.
 */
class MemoryStorage implements Storage {
  #items = new Map<string, string>();

  get length() {
    return this.#items.size;
  }
  clear() {
    this.#items.clear();
  }
  getItem(key: string) {
    return this.#items.get(key) ?? null;
  }
  key(index: number) {
    return [...this.#items.keys()][index] ?? null;
  }
  removeItem(key: string) {
    this.#items.delete(key);
  }
  setItem(key: string, value: string) {
    this.#items.set(key, String(value));
  }
}

const storage = new MemoryStorage();
for (const target of [globalThis, globalThis.window].filter(Boolean)) {
  Object.defineProperty(target, "localStorage", { value: storage, configurable: true });
}

// A locale left over from one test must not decide what the next one renders.
beforeEach(() => storage.clear());
