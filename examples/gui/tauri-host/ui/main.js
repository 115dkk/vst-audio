const { invoke } = window.__TAURI__.core;
const element = (id) => document.getElementById(id);

function show(view) {
  if (!view) return;
  element("plugin").textContent = view.pluginName;
  element("status").textContent = view.status;
  element("open").disabled = !view.canOpen;
  element("close").disabled = !view.canClose;
  element("detail").hidden = !view.detail;
  element("detail").textContent = view.detail ?? "";
}

element("open").addEventListener("click", () => invoke("open_editor").then(show));
element("close").addEventListener("click", () => invoke("close_editor").then(show));
element("quit").addEventListener("click", () => invoke("quit"));

// The model advances only while the page polls it, like a GUI frame clock.
async function tick() {
  try {
    show(await invoke("tick"));
  } catch (error) {
    console.error(error);
  }
  setTimeout(tick, 16);
}
tick();
