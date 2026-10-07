// Browser frontend for iroh-share. All interface logic lives in the wasm module
// (src/app.rs and src/view.rs): it sends the whole page as one view object after
// every change, and this file only turns that into DOM, forwards user intents,
// and does what only a page can do (clipboard, opening links, saving files).

const $ = (selector) => document.querySelector(selector);

let app = null;

function dispatch(intent) {
  try {
    app?.dispatch(intent);
  } catch (error) {
    console.error(error);
    setStatus(error?.message ?? String(error));
  }
}

function setStatus(text) {
  $("#status").textContent = text;
}

// ---------------------------------------------------------------------------
// Rendering

function render(view) {
  // Rebuilt rows replace their inputs; keep focus and selection on the same key.
  const focus = captureFocus();
  $("#spinner").hidden = !view.busy;
  setStatus(view.status);
  $("#pairing").hidden = !view.pairing;
  $("#app").hidden = !!view.pairing;
  if (view.pairing) {
    renderPairing(view.pairing);
  } else {
    renderTop(view);
    $("#page-data").hidden = view.page !== "data";
    $("#page-settings").hidden = view.page !== "settings";
    renderData(view);
    renderSettings(view.settings);
  }
  renderConfirm(view.confirm);
  restoreFocus(focus);
}

function renderPairing(p) {
  $("#pairing-title").textContent = p.title;
  setValue($("#pair-ticket"), p.ticket);
  setValue($("#pair-name"), p.name);
  for (const id of ["#pair-ticket", "#pair-name", "#pair-connect"]) $(id).disabled = p.busy;
  $("#pair-cancel").hidden = !p.canCancel;
}

function renderTop(view) {
  $("#daemon-title").textContent = view.daemonTitle;
  const list = $("#daemon-list");
  list.replaceChildren(
    ...view.daemons.map((d) => {
      const b = button(d.label, false, () => {
        $("#daemon-menu").open = false;
        dispatch({ type: "switchDaemon", id: d.id });
      });
      b.title = d.id;
      b.classList.toggle("selected", d.selected);
      return b;
    }),
    el("hr"),
    button("Add daemon…", false, () => {
      $("#daemon-menu").open = false;
      dispatch({ type: "addDaemon" });
    }),
  );
  for (const tab of document.querySelectorAll(".tab")) {
    tab.classList.toggle("active", tab.dataset.page === view.page);
  }
}

function renderData(view) {
  $("#jobs").replaceChildren(...view.jobs.map(jobRow));
  renderAddRow(view.add);
  $("#other-names").hidden = !view.otherNames;
  if (view.otherNames) $("#other-names-body").replaceChildren(...nameRows(view.otherNames));
  $("#names-body").replaceChildren(...nameRows(view.names));
  $("#export-all").disabled = !view.canNamesIo;
  $("#import-names").disabled = !view.canNamesIo;
}

function jobRow(job) {
  const path = el("button", { type: "button", class: "path-button", title: job.path }, job.path);
  path.classList.toggle("selected", job.selected);
  path.addEventListener("click", () => dispatch({ type: "select", id: job.id }));
  const links = td("c-links", ...job.names.map(nameLink));
  if (job.seeding) {
    links.append(
      line(
        ...linkContents(job.seeding.url, job.seeding.compact, "link"),
        copyIcon("ticket", job.seeding.ticket, "Copy ticket, for sendme and compatible apps"),
        copyIcon("hash", job.seeding.hash, "Copy BLAKE3 hash (hex)"),
      ),
    );
  } else {
    links.append(line(el("span", { class: "weak" }, "Content link available after import")));
  }
  return el(
    "tr",
    {},
    td("c-path", line(path)),
    td("c-state", line(el("span", { class: "truncate", title: job.status }, job.status))),
    links,
    td(
      "c-actions",
      line(
        button("Add name", !job.canAddName, () => dispatch({ type: "addName", id: job.id })),
        button("Update…", !job.canUpdate, () => dispatch({ type: "updateData", id: job.id })),
        iconButton("trash", "Remove data; files are kept", !job.canRemove, () =>
          dispatch({ type: "removeData", id: job.id }),
        ),
      ),
    ),
  );
}

function renderAddRow(add) {
  $("#add-mode").value = add.mode;
  for (const node of document.querySelectorAll(".add-row [class*='mode-']")) {
    node.hidden = !node.classList.contains(`mode-${add.mode}`);
  }
  const path = $("#add-path");
  setValue(path, add.path);
  path.placeholder = add.pathPlaceholder;
  setValue($("#add-ticket"), add.ticket);
  setValue($("#add-source"), add.source);
  $("#add-include-dir").checked = add.includeDirectoryName;
  $("#add-discover").checked = add.discover;
  $("#updating").textContent = add.updating ?? "";
  const submit = $("#add-submit");
  submit.textContent = add.submitLabel;
  submit.disabled = !add.canSubmit;
  submit.title = add.reason ?? "";
  $("#add-cancel").hidden = !add.canCancel;
  const list = $("#candidates");
  list.replaceChildren(
    ...add.candidates.map((candidate) => {
      const item = el("li", { title: candidate }, candidate);
      // mousedown, so the path field keeps focus.
      item.addEventListener("mousedown", (e) => {
        e.preventDefault();
        dispatch({ type: "chooseCandidate", path: candidate });
      });
      return item;
    }),
  );
  list.hidden = add.candidates.length === 0;
}

function nameRows(table) {
  const rows = table.rows.map((row) => (row.editor ? editorRow(row, table.content) : nameRow(row, table.content)));
  const key = `new-records-${table.content}`;
  const area = textarea(key, table.newRecords, "newRecords");
  rows.push(
    el(
      "tr",
      {},
      td("c-name", el("span", { class: "weak" }, "New name (advanced DNS)")),
      td("c-records", area),
      td("c-actions", line(button("Add name", !table.canCreate, () => dispatch({ type: "createRecordsName" })))),
    ),
  );
  return rows;
}

function nameRow(row, content) {
  const records = el("pre", { class: "records truncate", title: row.records }, row.records);
  records.classList.toggle("weak", row.managed);
  const edit = button("Edit", !row.canEdit, () => dispatch({ type: "editName", label: row.label, content }));
  edit.title = row.editTitle;
  const exportOne = button("Export pkarr…", !row.canExport, () =>
    dispatch({ type: "exportName", label: row.label }),
  );
  exportOne.title = "Save a ZIP with this name's private signing key and current signed record.";
  return el(
    "tr",
    {},
    td("c-name", nameLink(row.link)),
    td("c-records", records),
    td(
      "c-actions",
      line(
        edit,
        exportOne,
        iconButton("trash", row.removeTitle, !row.canRemove, () =>
          dispatch({ type: "removeName", label: row.label }),
        ),
      ),
    ),
  );
}

function editorRow(row) {
  const editor = row.editor;
  let field;
  if (editor.kind === "job") {
    field = el("select", { "data-key": "editor" });
    if (editor.selected == null) field.append(el("option", { value: "" }, "Select data to follow"));
    for (const option of editor.options) {
      const node = el("option", { value: String(option.id) }, option.path);
      node.selected = option.id === editor.selected;
      field.append(node);
    }
    field.addEventListener("change", () =>
      dispatch({ type: "setEditorJob", id: field.value === "" ? null : Number(field.value) }),
    );
  } else {
    field = textarea("editor", editor.text, "editorText");
  }
  return el(
    "tr",
    {},
    td("c-name", nameLink(row.link)),
    td("c-records", field),
    td(
      "c-actions",
      line(
        button("Save", !editor.canSave, () => dispatch({ type: "saveName" })),
        button("Cancel", false, () => dispatch({ type: "cancelEdit" })),
      ),
    ),
  );
}

// A name as its public URL with copy/open actions, optional remove, and status.
function nameLink(link) {
  const parts = linkContents(link.url, link.compact, "name");
  if (link.canRemove != null) {
    parts.push(
      iconButton("trash", "Remove name…", !link.canRemove, () =>
        dispatch({ type: "removeName", label: link.label }),
      ),
    );
  }
  parts.push(el("span", { class: "weak truncate", title: link.status }, link.status));
  return line(...parts);
}

function renderSettings(settings) {
  $("#import-directory").textContent = settings.importDirectory ?? "";
  $("#daemon-settings").hidden = !settings.daemon;
  if (settings.daemon) {
    $("#daemon-id").textContent = settings.daemon.id;
    const name = $("#daemon-name");
    name.placeholder = settings.daemon.short;
    setValue(name, settings.daemon.name);
  }
  $("#client-id").textContent = settings.clientId;
}

function renderConfirm(confirm) {
  const dialog = $("#confirm");
  if (!confirm) {
    if (dialog.open) dialog.close();
    return;
  }
  $("#confirm-text").textContent = confirm.text;
  $("#confirm-ok").disabled = !confirm.canConfirm;
  if (!dialog.open) dialog.showModal();
}

// ---------------------------------------------------------------------------
// Browser-only effects requested by the wasm side

function onEffect(effect) {
  switch (effect.type) {
    case "saveFile": {
      const url = URL.createObjectURL(new Blob([effect.bytes], { type: "application/zip" }));
      const a = el("a", { href: url, download: effect.name });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
      break;
    }
    case "focus": {
      const input = document.getElementById(effect.key);
      if (!input) break;
      input.focus();
      if (effect.cursorEnd) input.setSelectionRange(input.value.length, input.value.length);
      break;
    }
  }
}

async function copyText(value) {
  try {
    await navigator.clipboard.writeText(value);
    return true;
  } catch (error) {
    setStatus(`Cannot copy: ${error?.message ?? error}`);
    return false;
  }
}

// ---------------------------------------------------------------------------
// DOM helpers and icons

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
  node.append(...children);
  return node;
}
function td(cls, ...children) {
  return el("td", { class: cls }, ...children);
}
function line(...children) {
  return el("div", { class: "line" }, ...children);
}
function button(label, disabled, onClick) {
  const b = el("button", { type: "button" }, label);
  b.disabled = disabled;
  b.addEventListener("click", onClick);
  return b;
}
function textarea(key, value, field) {
  const area = el("textarea", {
    rows: 4,
    spellcheck: "false",
    placeholder: "@ 300 IN HTTPS 0 example.com.",
    "data-key": key,
  });
  area.value = value;
  area.addEventListener("input", () => dispatch({ type: "setText", field, value: area.value }));
  return area;
}
// Only touch values that changed, so the cursor stays where the user typed.
function setValue(input, value) {
  if (input.value !== value) input.value = value;
}

function captureFocus() {
  const active = document.activeElement;
  const key = active?.dataset?.key;
  if (!key) return null;
  return { key, start: active.selectionStart, end: active.selectionEnd, scroll: active.scrollTop };
}
function restoreFocus(focus) {
  if (!focus) return;
  const node = document.querySelector(`[data-key="${focus.key}"]`);
  if (!node || node === document.activeElement) return;
  node.focus();
  if (focus.start != null && node.setSelectionRange) node.setSelectionRange(focus.start, focus.end);
  node.scrollTop = focus.scroll ?? 0;
}

const ICONS = {
  // A blake3.net link to one version: two chain links.
  link: '<rect x="0.5" y="5" width="10" height="6" rx="3"/><rect x="5.5" y="5" width="10" height="6" rx="3"/>',
  // A pkarr.net link to a name: a luggage tag.
  name: '<path d="M1 4h9l5 4-5 4H1z"/><circle cx="10.5" cy="8" r="1.2"/>',
  ticket: '<rect x="1" y="3" width="14" height="10" rx="1"/><path d="M10 4.5v1.5M10 7v1.5M10 9.5v1.5"/>',
  hash: '<path d="M6 2 4 14M12 2l-2 12M2 6h12M1.5 10h12"/>',
  check: '<path d="m2 8 4 4 8-9"/>',
  open: '<path d="M2 6v8h8v-4M2 6h4M7 9l7-7M8 2h6v6"/>',
  trash: '<path d="M2 4h12M6 1h4v3M4 6v9h8V6M7 7v5M9 7v5"/>',
};
function svg(icon) {
  return `<svg viewBox="0 0 16 16" aria-hidden="true">${ICONS[icon]}</svg>`;
}

function iconButton(icon, label, disabled, onClick) {
  const b = el("button", { type: "button", class: "icon", title: label, "aria-label": label });
  b.innerHTML = svg(icon);
  b.disabled = disabled;
  b.addEventListener("click", onClick);
  return b;
}

// An icon that copies `value` and shows a check mark for two seconds.
function copyIcon(icon, value, label) {
  const b = iconButton(icon, label, false, async () => {
    if (!(await copyText(value))) return;
    b.innerHTML = svg("check");
    b.title = "Copied!";
    setTimeout(() => {
      b.innerHTML = svg(icon);
      b.title = label;
    }, 2000);
  });
  return b;
}

// The compact label copies the full URL; opening is always an explicit icon.
function linkContents(url, compact, icon) {
  const label = el("button", { type: "button", class: "compact mono", title: "Click to copy URL" }, compact);
  label.addEventListener("click", async () => {
    if (!(await copyText(url))) return;
    label.textContent = "Copied!";
    setTimeout(() => (label.textContent = compact), 2000);
  });
  const copyLabel = icon === "name" ? "Copy name link, always the current version" : "Copy link to this version";
  return [
    label,
    copyIcon(icon, url, copyLabel),
    iconButton("open", "Open URL in browser", false, () => window.open(url, "_blank", "noopener")),
  ];
}

// ---------------------------------------------------------------------------
// Wiring: every control forwards an intent

const MAX_TICKET_FILE = 16384;

// Reads what a drop offers; the wasm side decides what to do with it.
async function onDrop(event) {
  event.preventDefault();
  document.body.classList.remove("dragging");
  const files = await Promise.all(
    [...(event.dataTransfer?.files ?? [])].map(async (file) => ({
      name: file.name,
      mime: file.type,
      text: file.size <= MAX_TICKET_FILE ? await file.text().catch(() => null) : null,
    })),
  );
  const text = files.length === 0 ? (event.dataTransfer?.getData("text/plain") ?? null) : null;
  dispatch({ type: "drop", files, text });
}

function wire() {
  const text = (id, field) =>
    $(id).addEventListener("input", (e) => dispatch({ type: "setText", field, value: e.target.value }));
  const flag = (id, field) =>
    $(id).addEventListener("change", (e) => dispatch({ type: "setFlag", field, value: e.target.checked }));
  const click = (id, type) => $(id).addEventListener("click", () => dispatch({ type }));

  text("#pair-ticket", "pairTicket");
  text("#pair-name", "pairName");
  text("#add-path", "addPath");
  text("#add-ticket", "addTicket");
  text("#add-source", "addSource");
  text("#daemon-name", "daemonName");
  flag("#add-include-dir", "includeDirectoryName");
  flag("#add-discover", "discover");
  click("#pair-connect", "pair");
  click("#pair-cancel", "cancelPairing");
  click("#add-submit", "submitAdd");
  click("#add-cancel", "cancelUpdate");
  click("#export-all", "exportAll");
  click("#daemon-rename", "renameDaemon");
  click("#daemon-forget", "forgetDaemon");
  click("#confirm-ok", "confirm");
  click("#confirm-cancel", "cancelConfirm");
  $("#confirm").addEventListener("cancel", () => dispatch({ type: "cancelConfirm" }));
  $("#pair-ticket").addEventListener("keydown", (e) => e.key === "Enter" && dispatch({ type: "pair" }));
  $("#add-mode").addEventListener("change", (e) => dispatch({ type: "setMode", mode: e.target.value }));
  for (const tab of document.querySelectorAll(".tab")) {
    tab.addEventListener("click", () => dispatch({ type: "setPage", page: tab.dataset.page }));
  }
  // Tab completes against the daemon's filesystem; Escape dismisses candidates.
  $("#add-path").addEventListener("keydown", (e) => {
    if (e.key === "Tab" && !e.shiftKey && !e.altKey && !e.ctrlKey && !e.metaKey) {
      e.preventDefault();
      dispatch({ type: "complete" });
    } else if (e.key === "Escape") {
      dispatch({ type: "dismissCandidates" });
    }
  });
  document.addEventListener("click", (e) => {
    const menu = $("#daemon-menu");
    if (menu.open && !menu.contains(e.target)) menu.open = false;
    if (!$("#candidates").hidden && !$(".path-field").contains(e.target)) {
      dispatch({ type: "dismissCandidates" });
    }
  });
  $("#import-names").addEventListener("click", () => $("#import-file").click());
  $("#import-file").addEventListener("change", async (e) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    try {
      app?.import_names(new Uint8Array(await file.arrayBuffer()));
    } catch (error) {
      setStatus(`Cannot read archive: ${error?.message ?? error}`);
    }
  });
  document.addEventListener("dragover", (e) => {
    e.preventDefault();
    if ($("#pairing").hidden) document.body.classList.add("dragging");
  });
  document.addEventListener("dragleave", (e) => {
    if (e.relatedTarget == null) document.body.classList.remove("dragging");
  });
  document.addEventListener("drop", onDrop);
  document.addEventListener("visibilitychange", () =>
    dispatch({ type: "visibility", hidden: document.visibilityState === "hidden" }),
  );
}

async function boot() {
  wire();
  try {
    // Resolve the wasm relative to this page, with or without a trailing slash.
    let dir = location.pathname;
    if (!dir.endsWith("/")) {
      dir = dir.endsWith(".html") ? dir.slice(0, dir.lastIndexOf("/") + 1) : `${dir}/`;
    }
    const wasm = await import(`${dir}wasm/iroh_share_web.js`);
    await wasm.default();
    app = await wasm.App.start(render, onEffect);
  } catch (error) {
    setStatus(`Failed to start: ${error?.message ?? error}`);
    console.error(error);
  }
}

boot();
