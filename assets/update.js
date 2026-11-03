// Reloads the page once the panel runs another version, while an update installs. The
// panel restarts on the new version, and the page the old one rendered would keep its
// styles and scripts, even where the runtime renders parts of it again.
//
// The sidebar names the version that rendered the page, and the URL that tells the one
// running now. An element marked `data-update-running` shows while an update installs.
const sidebar = document.querySelector("[data-panel-version]");
const rendered = sidebar?.dataset.panelVersion;
const versionUrl = sidebar?.dataset.versionUrl;
let polling = false;

const poll = async () => {
  try {
    const response = await fetch(versionUrl, { cache: "no-store" });
    const running = (await response.text()).trim();
    if (response.ok && running !== rendered) {
      location.reload();
      return;
    }
  } catch {
    // The panel is restarting.
  }
  if (document.querySelector("[data-update-running]")) {
    setTimeout(poll, 1500);
  } else {
    polling = false;
  }
};

const watch = () => {
  if (versionUrl && !polling && document.querySelector("[data-update-running]")) {
    polling = true;
    poll();
  }
};

new MutationObserver(watch).observe(document.body, { childList: true, subtree: true });
watch();
