// Shows the readings under the pointer on the monitoring charts: a crosshair at the
// nearest sample and a box with its values. The server replaces the charts after every
// sample, so the readout is drawn again whenever they change.
let pointer = null;
let cache = { raw: null, samples: [] };

const isReadout = (node) => node instanceof Element && node.hasAttribute("data-readout");

const samplesFor = (plot) => {
  const raw = plot.closest("[data-samples]")?.dataset.samples ?? null;
  if (raw !== cache.raw) {
    cache = { raw, samples: raw ? JSON.parse(raw) : [] };
  }
  return cache.samples;
};

const element = (tag, className, text) => {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

const clear = () => {
  for (const node of document.querySelectorAll("[data-readout]")) node.remove();
};

const draw = () => {
  clear();
  if (!pointer) return;
  const plot = document.elementFromPoint(pointer.x, pointer.y)?.closest("[data-plot]");
  if (!plot) return;
  const samples = samplesFor(plot);
  if (samples.length === 0) return;

  const box = plot.getBoundingClientRect();
  const width = Number(plot.dataset.width);
  const start = Number(plot.dataset.start);
  const position = Math.round(((pointer.x - box.left) / box.width) * width);
  // Left of the oldest sample, the chart has no data yet.
  if (position < start) return;
  const index = Math.min(position - start, samples.length - 1);
  const sample = samples[index];
  const x = ((start + index) / width) * 100;

  const crosshair = element("div", "pointer-events-none absolute inset-y-0 w-px bg-foreground/30");
  crosshair.dataset.readout = "";
  crosshair.style.left = `${x}%`;

  const readout = element(
    "div",
    "pointer-events-none absolute top-0 z-10 flex flex-col gap-1 rounded-md border border-border bg-popover px-2.5 py-1.5 text-xs whitespace-nowrap text-popover-foreground shadow-sm",
  );
  readout.dataset.readout = "";
  // Beside the crosshair, on whichever side keeps the box inside the chart.
  if (x < 50) readout.style.left = `calc(${x}% + 8px)`;
  else readout.style.right = `calc(${100 - x}% + 8px)`;

  readout.append(element("span", "text-muted-foreground", sample.time));
  const colors = plot.dataset.colors.split(",");
  sample[plot.dataset.fields].forEach(([label, value], series) => {
    const row = element("span", "flex items-center gap-1.5");
    row.append(
      element("span", `h-0.5 w-3 rounded-full bg-current ${colors[series]}`),
      element("span", "font-medium tabular-nums", value),
      element("span", "text-muted-foreground", label),
    );
    readout.append(row);
  });

  plot.append(crosshair, readout);
};

document.addEventListener("pointermove", (event) => {
  pointer = { x: event.clientX, y: event.clientY };
  draw();
});

document.documentElement.addEventListener("pointerleave", () => {
  pointer = null;
  clear();
});

// Redraw after the server replaces the charts, ignoring the readout's own changes.
new MutationObserver((records) => {
  const replaced = records.some((record) =>
    [...record.addedNodes, ...record.removedNodes].some((node) => !isReadout(node)),
  );
  if (replaced && pointer) requestAnimationFrame(draw);
}).observe(document.body, { childList: true, subtree: true });
