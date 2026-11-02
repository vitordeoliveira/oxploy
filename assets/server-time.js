// Keeps the server time badges ticking. The server renders each badge's time once,
// along with the time it rendered and its offset from UTC; this advances the clock
// from there.
for (const badge of document.querySelectorAll("[data-server-time]")) {
  const renderedAt = Number(badge.dataset.serverTime);
  const offset = Number(badge.dataset.utcOffset) * 1000;
  const loadedAt = performance.now();
  const pad = (value) => String(value).padStart(2, "0");

  const tick = () => {
    // Shifting by the offset and reading the UTC fields gives the server's wall clock.
    const now = new Date(renderedAt + offset + (performance.now() - loadedAt));
    badge.textContent = `${pad(now.getUTCHours())}:${pad(now.getUTCMinutes())}:${pad(now.getUTCSeconds())}`;
  };

  tick();
  setInterval(tick, 500);
}
