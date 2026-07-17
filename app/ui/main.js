// org desktop UI. Plain JS, no framework: state is "which person, which pane",
// and every render pulls fresh data over IPC so the view can't go stale.

const { invoke } = window.__TAURI__.core;

const $ = (sel) => document.querySelector(sel);

// --- pane switching ---------------------------------------------------------

const PANES = ["empty", "detail", "tree", "error"];

function showPane(name) {
  for (const p of PANES) {
    $(`#${p}`).hidden = p !== name;
  }
}

function showError(e) {
  $("#error-msg").textContent = String(e);
  showPane("error");
}

// --- shared renderers ---------------------------------------------------------

// One clickable person line: "#id Name — Title".
function personItem(id, name, title) {
  const li = document.createElement("li");
  const idSpan = document.createElement("span");
  idSpan.className = "person-id";
  idSpan.textContent = `#${id}`;
  const link = document.createElement("a");
  link.className = "person-link";
  link.textContent = name;
  link.addEventListener("click", () => showPerson(id));
  li.append(idSpan, link);
  if (title) {
    const t = document.createElement("span");
    t.className = "person-title";
    t.textContent = ` — ${title}`;
    li.append(t);
  }
  return li;
}

function fillList(listEl, nodes, emptyText) {
  listEl.replaceChildren();
  if (nodes.length === 0) {
    const li = document.createElement("li");
    li.className = "muted";
    li.textContent = emptyText;
    listEl.append(li);
    return;
  }
  for (const n of nodes) {
    const li = personItem(n.id, n.name, n.title);
    if (n.depth > 1) {
      li.style.paddingLeft = `${(n.depth - 1) * 18}px`;
    }
    listEl.append(li);
  }
}

// --- search sidebar -----------------------------------------------------------

let searchTimer;

async function runSearch(query) {
  try {
    const hits = await invoke("search", { query });
    const ul = $("#results");
    ul.replaceChildren();
    for (const h of hits) {
      const li = document.createElement("li");
      const name = document.createElement("div");
      name.className = "name";
      name.textContent = h.person.name;
      const sub = document.createElement("div");
      sub.className = "sub";
      sub.textContent = [h.person.title, h.person.team]
        .filter(Boolean)
        .join(" · ");
      li.append(name, sub);
      li.addEventListener("click", () => showPerson(h.person.id));
      ul.append(li);
    }
  } catch (e) {
    showError(e);
  }
}

$("#search").addEventListener("input", (ev) => {
  // Debounce: search-as-you-type without a query per keystroke.
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => runSearch(ev.target.value), 150);
});

// --- person detail --------------------------------------------------------------

let currentId = null;

async function showPerson(id) {
  try {
    const view = await invoke("who", { id });
    currentId = id;
    const p = view.person;
    $("#d-name").textContent = `#${p.id}  ${p.name}`;
    $("#d-title").textContent = p.title ?? "";
    $("#d-team").textContent = p.team ?? "";
    $("#d-notes-wrap").hidden = !p.notes;
    $("#d-notes").textContent = p.notes ?? "";
    fillList($("#d-chain"), view.chain, "reports to no one — this is a root");
    fillList($("#d-reports"), view.reports, "none");
    showPane("detail");
  } catch (e) {
    showError(e);
  }
}

// --- tree -----------------------------------------------------------------------

async function showTree(id) {
  try {
    const view = await invoke("tree", { id });
    const a = view.anchor;
    $("#t-name").textContent = `#${a.id}  ${a.name}`;
    // Anchor at depth 0, then the pre-ordered subtree; indent by depth.
    const nodes = [
      { id: a.id, name: a.name, title: a.title, depth: 0 },
      ...view.nodes,
    ].map((n) => ({ ...n, depth: n.depth + 1 })); // shift so fillList indents depth>1
    fillList($("#t-nodes"), nodes, "nobody");
    showPane("tree");
  } catch (e) {
    showError(e);
  }
}

$("#d-tree-btn").addEventListener("click", () => {
  if (currentId !== null) showTree(currentId);
});

$("#t-back-btn").addEventListener("click", () => {
  if (currentId !== null) showPerson(currentId);
});

// --- boot -----------------------------------------------------------------------

// Empty query lists everyone (seniority-ordered from core).
runSearch("");
