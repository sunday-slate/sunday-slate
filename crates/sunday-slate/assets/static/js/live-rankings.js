(() => {
  const inbox = document.querySelector('[data-rankings-inbox][hx-ext~="sse"]');
  if (!inbox) return;

  const rows = (root) => Array.from(root.querySelector('[data-rankings-list]')?.children ?? []);
  const quietMs = 200;
  const pressed = new Set();
  let latest, seen, timer;
  let lastScroll = -Infinity;

  function settle() {
    timer = undefined;
    if (!latest || pressed.size) return;
    const snapshot = latest;
    latest = undefined;
    const region = document.getElementById(snapshot.dataset.rankingsSnapshot);
    if (!region) return;
    const scrollY = window.scrollY;
    const content = snapshot.content;
    const next = new Map(rows(content).map((row) => [row.dataset.entryId, row]));
    let anchor, top;
    if (scrollY > 0) {
      const headerBottom = document.querySelector('body > header')?.getBoundingClientRect().bottom ?? 0;
      for (const row of rows(region)) {
        const replacement = next.get(row.dataset.entryId);
        if (!replacement) continue;
        const rect = row.getBoundingClientRect();
        if (rect.bottom > headerBottom && rect.top < innerHeight) {
          anchor = replacement;
          top = rect.top;
          break;
        }
      }
    }
    const active = document.activeElement;
    const focusedId = region.contains(active) ? active.closest('[data-entry-id]')?.dataset.entryId : undefined;

    region.replaceChildren(content);
    next.get(focusedId)?.querySelector('a[href]')?.focus({ preventScroll: true });
    const target = anchor ? window.scrollY + anchor.getBoundingClientRect().top - top : scrollY;
    if (target !== window.scrollY) window.scrollTo({ top: target, behavior: 'instant' });
  }

  function schedule() {
    if (timer !== undefined) clearTimeout(timer);
    if (latest && !pressed.size) {
      timer = setTimeout(settle, Math.max(0, quietMs - (performance.now() - lastScroll)));
    }
  }

  function handleScroll() {
    lastScroll = performance.now();
    if (latest) schedule();
  }
  window.addEventListener('scroll', handleScroll, { passive: true });
  window.addEventListener('touchmove', handleScroll, { passive: true });

  window.addEventListener('pointerdown', (event) => {
    if (event.target.closest?.('[data-rankings-list] a[href]')) pressed.add(event.pointerId);
  }, { capture: true, passive: true });
  function release(event) {
    // Schedule rather than swapping during pointerup, before the click.
    if (pressed.delete(event.pointerId)) schedule();
  }
  window.addEventListener('pointerup', release, { capture: true, passive: true });
  window.addEventListener('pointercancel', release, { capture: true, passive: true });
  window.addEventListener('blur', () => {
    if (!pressed.size) return;
    pressed.clear();
    schedule();
  });

  function receive() {
    const frame = inbox.querySelector('template[data-rankings-snapshot]');
    if (!frame || frame === seen) return;
    seen = latest = frame;
    schedule();
  }

  new MutationObserver(receive).observe(inbox, { childList: true });
  receive();
})();
