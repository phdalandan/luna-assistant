const IDLE_MS = 800;
const timers = new WeakMap<Element, number>();

/** Marks a scrolling element with `scrolling` until it has been idle, so its scrollbar can hide. */
export function showScrollbarsWhileScrolling(): void {
  document.addEventListener(
    "scroll",
    (event) => {
      const element = event.target;
      if (!(element instanceof Element)) return;
      element.classList.add("scrolling");
      window.clearTimeout(timers.get(element));
      timers.set(
        element,
        window.setTimeout(() => element.classList.remove("scrolling"), IDLE_MS),
      );
    },
    { capture: true, passive: true },
  );
}
