// The overview receives finished rows, scopes, attention, and copy from Rust.
// Tile edits are persisted before they appear; polling never replaces a draft
// form. Existing detailed renderers remain the destination for deeper work.
(function () {
  const root = document.getElementById("dashboardOverview");
  const legacy = document.getElementById("cockpitView");
  const sourcePanels = {
    containers: "containersPanel",
    ghWorkflows: "reposPanel",
    ghRunners: "runnersPanel",
    claudeUsage: "usagePanel",
    azureCost: "azurePanel",
    services: "servicesPanel",
    sentryCrons: "cronsPanel",
    openclawAgents: "openclawPanel",
  };
  let model = null,
    editing = false,
    busy = false,
    pending = false,
    active = null;
  let mode =
    new URLSearchParams(location.search).get("view") === "details"
      ? "details"
      : "overview";
  let history = [],
    dragged = null,
    initialized = false;
  let pointerDrag = null;
  let loadFailed = false;
  let previewTimer = null, previewVersion = 0;
  const tiles = new Map();
  const q = (selector) => root.querySelector(selector);
  const text = (el, value) => {
    el.textContent = value ?? "";
    return el;
  };
  function node(tag, cls, value) {
    const el = document.createElement(tag);
    if (cls) el.className = cls;
    if (value != null) text(el, value);
    return el;
  }
  const L = (key) => model?.labels?.[key] ?? "";
  function button(label, action, id, cls = "") {
    const b = node("button", cls, label);
    b.type = "button";
    b.dataset.action = action;
    if (id) b.dataset.id = id;
    return b;
  }
  function status(message, error = false) {
    const el = q(".db-live-note");
    if (!el) return;
    text(el, message);
    el.classList.toggle("db-save-error", error);
    el.setAttribute("role", error ? "alert" : "status");
  }
  function source(id) {
    return model.sources.find((s) => s.id === id);
  }
  function tile(id) {
    return model.layout.tiles.find((t) => t.id === id);
  }
  function colored(tag, cls, value, color) {
    const el = node(tag, cls, value);
    if (color) el.style.color = color;
    return el;
  }
  function actionLabel(b, title) {
    b.setAttribute("aria-label", `${b.textContent} ${title}`);
    return b;
  }
  function makeChrome() {
    const chrome = node("header", "db-chrome"),
      brand = node("div", "db-brand");
    const mark = node("img", "brandmark");
    mark.src = "mark.svg";
    mark.alt = "";
    const title = node("div");
    title.append(
      node("h1", "", L("title")),
      node("p", "db-sub", L("subtitle")),
    );
    brand.append(mark, title);
    const actions = node("div", "db-actions");
    if (!window.__TAURI__)
      actions.append(node("span", "db-sample", L("preview")));
    actions.append(
      button(L("add"), "catalog"),
      button(L("undo"), "undo"),
      button(L("edit"), "edit"),
      button(L("settings"), "settings"),
    );
    chrome.append(brand, actions);
    const attention = node("section", "db-attention");
    attention.setAttribute("aria-label", L("attention"));
    const head = node("div", "db-attention-head");
    head.append(
      node("h2", "", L("attention")),
      node("span", "db-sub", L("attentionNote")),
    );
    attention.append(head, node("div", "db-attention-items"));
    const editbar = node("div", "db-editbar");
    editbar.append(
      node("span", "", L("editHint")),
      node("span", "db-hidden-count"),
    );
    const inspector = node("section", "db-inspector");
    inspector.id = "dashboardInspector";
    inspector.hidden = true;
    inspector.setAttribute("aria-label", L("configure"));
    const grid = node("main", "db-grid");
    grid.setAttribute("aria-label", L("subtitle"));
    const footer = node("footer", "db-end");
    const live = node("span", "db-live-note");
    live.setAttribute("role", "status");
    live.setAttribute("aria-live", "polite");
    footer.append(live, button(L("allPanels"), "allPanels", null, "db-plain"));
    root.replaceChildren(chrome, attention, editbar, inspector, grid, footer);
    const back = button(L("back"), "back");
    back.id = "dashboardBack";
    back.className = "btn";
    legacy.querySelector(".topbar").prepend(back);
    back.addEventListener("click", () => showOverview());
    initialized = true;
  }
  function warnings(values) {
    const wrap = node("div", "db-warnings");
    for (const v of values || [])
      wrap.append(colored("p", "db-warning", v.text, v.color));
    return wrap;
  }
  function makeRow(row, t, detail = false, interactive = true) {
    const wrap = node(
      "div",
      row.metrics?.length ? "db-host" : "db-compact-row",
    );
    const b = interactive ? button("", "row", row.id, "db-item") : node("div", "db-item");
    b.dataset.source = t.source;
    const name = node("span", "db-row-label"),
      dot = node("span", "db-dot");
    dot.setAttribute("aria-hidden", "true");
    dot.style.color = row.color;
    name.append(dot, node("span", "db-item-name", row.label));
    b.append(name, colored("span", "db-value", row.value, row.valueColor));
    wrap.append(b);
    if (row.metrics?.length) {
      const stats = node("div", "db-host-stats");
      for (const m of row.metrics) {
        const label = node("div", "db-stat-label");
        label.append(
          node("span", "", m.label),
          node("strong", "", m.value ?? "—"),
        );
        stats.append(label);
      }
      wrap.append(stats);
    }
    const description = detail ? row.detail : row.compactDetail;
    if (description) wrap.append(node("p", "db-sub", description));
    if (detail && row.details) {
      const metrics = node("div", "db-extra-metrics");
      for (const m of row.details) {
        const r = node("div", "db-row");
        r.append(
          node("span", "db-muted", m.label),
          node("span", "", m.value ?? "—"),
        );
        metrics.append(r);
      }
      wrap.append(metrics);
    }
    return wrap;
  }
  function updateTiles(force = false) {
    const grid = q(".db-grid"),
      ids = new Set(model.tiles.map((t) => t.id));
    for (const [id, el] of tiles)
      if (!ids.has(id)) {
        el.remove();
        tiles.delete(id);
      }
    const empty = grid.querySelector(".db-empty");
    if (empty) empty.remove();
    if (!model.tiles.length) grid.append(node("p", "db-empty", L("empty")));
    model.tiles.forEach((t, index) => {
      let el = tiles.get(t.id),
        fresh = !el;
      if (!el) {
        el = node("article", "db-tile");
        el.dataset.tile = t.id;
        el.append(
          node("div", "db-tools"),
          node("header", "db-tile-head"),
          node("div", "db-tile-content"),
          node("footer", "db-tile-footer"),
        );
        tiles.set(t.id, el);
      }
      if (grid.children[index] !== el)
        grid.insertBefore(el, grid.children[index] || null);
      el.dataset.width = t.width;
      el.setAttribute("aria-label", t.title);
      el.classList.toggle("db-editable", editing);
      const tools = el.querySelector(".db-tools");
      tools.hidden = !editing;
      if (force || fresh) {
        const handle = node("span", "db-drag", `⠿ ${L("move")}`);
        tools.replaceChildren(handle);
        for (const [action, key, symbol, disabled] of [
          ["earlier", "earlier", "←", index === 0],
          ["later", "later", "→", index === model.tiles.length - 1],
          ["hide", "hide", L("hide"), false],
        ]) {
          const b = button(symbol, action, t.id);
          b.setAttribute("aria-label", `${L(key)} ${t.title}`);
          b.disabled = disabled || busy;
          tools.append(b);
        }
        const heading = node("div");
        heading.append(
          node("h2", "db-tile-title", t.title),
          node("p", "db-tile-note", `${t.scopeLabel} · ${L(t.presentation)}`),
        );
        const config = actionLabel(
          button(L("configure"), "configure", t.id),
          t.title,
        );
        config.hidden = !editing;
        config.disabled = busy;
        el.querySelector(".db-tile-head").replaceChildren(heading, config);
      }
      if (force || fresh || (!editing && !dragged)) {
        const content = el.querySelector(".db-tile-content");
        content.replaceChildren(warnings(t.warnings));
        if (!t.rows.length) {
          content.append(node("p", "db-sub", t.empty));
          if (t.emptyAction) {
            const action = button(L(t.emptyAction === "configure" ? "editScope" : "manage"), t.emptyAction, t.id, "db-empty-action");
            action.dataset.source = t.source;
            action.dataset.scope = tile(t.id).scope;
            content.append(action);
          }
        }
        for (const row of t.rows)
          content.append(makeRow(row, t, t.presentation === "detailed"));
        if (t.moreCount) {
          const more = button(t.moreLabel, "details", t.id, "db-more db-plain");
          content.append(more);
        }
        const footer = el.querySelector(".db-tile-footer");
        footer.replaceChildren(
          node("span", "", t.footer),
          button(L("details"), "details", t.id, "db-plain"),
        );
      }
    });
  }
  function render(force = false) {
    // Live readings may change while someone tabs through row buttons. Restore
    // the same logical control if painting replaced its DOM node.
    const focused = root.contains(document.activeElement)
      ? document.activeElement
      : null;
    const focusTile = focused?.closest("[data-tile]")?.dataset.tile;
    const focusData = focused?.dataset.action ? { ...focused.dataset } : null;
    if (!initialized) makeChrome();
    root.hidden = settingsOpen || mode !== "overview";
    legacy.hidden = settingsOpen || mode === "overview";
    overviewOpen = mode === "overview";
    const attention = q(".db-attention-items");
    attention.replaceChildren();
    for (const item of model.attention) {
      const b = button("", "attention", item.source),
        dot = node("span", "db-dot");
      dot.style.color = item.color;
      dot.setAttribute("aria-hidden", "true");
      b.append(dot, document.createTextNode(item.label));
      attention.append(b);
    }
    if (!model.attention.length)
      attention.append(
        node(
          "span",
          "db-muted",
          model.sources.some((s) => s.loading) ? L("loading") : L("quiet"),
        ),
      );
    q('[data-action="edit"]').textContent = editing ? L("done") : L("edit");
    q('[data-action="edit"]').classList.toggle("db-primary", editing);
    q('[data-action="edit"]').disabled = busy || !window.__TAURI__;
    q('[data-action="catalog"]').hidden = !editing;
    q('[data-action="undo"]').hidden = !editing;
    q('[data-action="catalog"]').disabled = busy;
    q('[data-action="undo"]').disabled = busy || !history.length;
    q(".db-editbar").hidden = !editing;
    text(
      q(".db-hidden-count"),
      `${model.layout.tiles.filter((t) => t.hidden).length} ${L("hidden")}`,
    );
    if (mode === "overview") updateTiles(force);
    if (active?.kind === "details") fillDetails();
    if (focused && !focused.isConnected && focusData) {
      const scope = focusTile
        ? root.querySelector(`[data-tile="${CSS.escape(focusTile)}"]`)
        : root;
      const replacement = [
        ...(scope?.querySelectorAll("button[data-action]") || []),
      ].find((button) =>
        Object.entries(focusData).every(
          ([key, value]) => button.dataset[key] === value,
        ),
      );
      replacement?.focus({ preventScroll: true });
    }
  }
  async function refresh(force = false) {
    if (pending || busy || settingsOpen) return;
    pending = true;
    try {
      const next = await callRust(
        "dashboard_view",
        { width: root.clientWidth || legacy.clientWidth || innerWidth },
        "sample-dashboard.json",
      );
      if (!next?.layout?.tiles || !Array.isArray(next.sources) || !next.labels)
        return;
      if (model && next.layout.revision < model.layout.revision) return;
      const configChanged =
        model && JSON.stringify(next.layout) !== JSON.stringify(model.layout);
      if (configChanged && !force) history = [];
      model = next;
      render(force || configChanged);
      if (loadFailed) {
        status("");
        loadFailed = false;
      }
      const error = document.getElementById("dashboardLoadError");
      if (error) error.remove();
    } catch (error) {
      if (initialized) {
        loadFailed = true;
        status(L("loadFailed"), true);
      } else if (
        mode === "overview" &&
        !document.getElementById("dashboardLoadError")
      ) {
        // A transport failure cannot receive its message from the failed IPC.
        const notice = node(
          "p",
          "dashboard-load-error",
          "Could not load the overview. Detailed readings are available below.",
        );
        notice.id = "dashboardLoadError";
        notice.setAttribute("role", "alert");
        legacy.prepend(notice);
      }
    } finally {
      pending = false;
    }
  }
  function selectField(name, label, choices, value) {
    const wrap = node("div", "db-field"),
      lab = node("label", "", label),
      input = node("select");
    input.name = name;
    input.id = `dashboard-${name}`;
    lab.htmlFor = input.id;
    for (const choice of choices) {
      const option = node("option", "", choice.label);
      option.value = choice.value;
      option.selected = choice.value === value;
      input.append(option);
    }
    wrap.append(lab, input);
    return wrap;
  }
  function openInspector(next) {
    active = next;
    const box = q(".db-inspector");
    box.hidden = false;
    box.replaceChildren();
    const head = node("div", "db-inspector-head"),
      copy = node("div");
    copy.append(node("h3"), node("p", "db-sub"));
    head.append(copy, button(L("close"), "close"));
    box.append(head, node("div", "db-inspector-body"));
    if (next.kind === "configure") {
      const t = next.draft || tile(next.id),
        s = source(t.source);
      text(copy.children[0], L(next.draft ? "add" : "configure"));
      text(copy.children[1], s.hint);
      const form = node("form");
      form.id = "dashboardForm";
      const fields = node("div", "db-fields");
      const name = node("div", "db-field"),
        label = node("label", "", L("name")),
        input = node("input");
      input.name = "title";
      input.id = "dashboard-title";
      input.type = "text";
      input.maxLength = 64;
      input.required = true;
      input.value = t.title;
      label.htmlFor = input.id;
      name.append(label, input);
      fields.append(name);
      const scopes = s.scopes.slice();
      if (!scopes.some((c) => c.value === t.scope))
        scopes.push({ value: t.scope, label: L("missingScope") });
      fields.append(
        selectField("scope", L("scope"), scopes, t.scope),
        selectField(
          "presentation",
          L("presentation"),
          ["summary", "detailed"].map((v) => ({ value: v, label: L(v) })),
          t.presentation,
        ),
        selectField(
          "width",
          L("width"),
          ["small", "medium", "wide"].map((v) => ({ value: v, label: L(v) })),
          t.width,
        ),
      );
      const actions = node("div", "db-form-actions");
      actions.append(
        button(L(next.draft ? "add" : "apply"), "apply", null, "db-primary"),
        button(L("manage"), "manage", t.source),
        node("span", "db-muted", L("scopeNote")),
      );
      if (!next.draft) actions.insertBefore(button(L("duplicate"), "duplicate", t.id), actions.children[1]);
      form.append(fields, actions);
      box.querySelector(".db-inspector-body").append(form);
      const preview = node("section", "db-preview");
      preview.setAttribute("aria-label", L("tilePreview"));
      preview.append(node("h3", "", L("tilePreview")), node("p", "db-sub", next.draft ? L("previewHint") : s.title), node("div", "db-preview-grid"));
      box.querySelector(".db-inspector-body").append(preview);
      form.addEventListener("input", schedulePreview);
      schedulePreview();
      input.focus();
    } else if (next.kind === "catalog") {
      text(copy.children[0], L("catalog"));
      text(copy.children[1], L("catalogHint"));
      const body = box.querySelector(".db-inspector-body"),
        catalog = node("div", "db-catalog");
      for (const t of model.layout.tiles.filter((t) => t.hidden))
        catalog.append(button(`${L("restore")} ${t.title}`, "restore", t.id));
      for (const s of model.sources) {
        const choose = button("", "add", s.id);
        choose.append(node("strong", "", s.title), node("span", "db-sub", s.hint));
        catalog.append(choose);
      }
      body.append(catalog);
    } else {
      const actions = node("div", "db-form-actions");
      actions.append(
        button(L("fullPanel"), "full", next.source),
        button(L("manage"), "manage", next.source),
      );
      box.append(actions);
      fillDetails();
      box.querySelector('[data-action="close"]').focus({ preventScroll: true });
    }
  }
  function formTile() {
    const form = q("#dashboardForm");
    if (!form || active?.kind !== "configure") return null;
    const original = active.draft || tile(active.id);
    if (!original) return null;
    const fields = new FormData(form);
    return { ...original, title: String(fields.get("title")).trim(), scope: fields.get("scope"), presentation: fields.get("presentation"), width: fields.get("width") };
  }
  function schedulePreview() {
    clearTimeout(previewTimer);
    const version = ++previewVersion;
    const target = q(".db-preview-grid");
    if (!target) return;
    if (!target.childElementCount) target.append(node("p", "db-sub", L("previewLoading")));
    target.setAttribute("aria-busy", "true");
    previewTimer = setTimeout(async () => {
      const draft = formTile();
      if (!draft) return;
      try {
        const next = await callRust("dashboard_preview", { tile: { ...draft, title: draft.title || source(draft.source).title }, width: root.clientWidth || innerWidth });
        if (version !== previewVersion || !target.isConnected) return;
        if (!next || !Array.isArray(next.rows)) throw new Error();
        const card = node("article", "db-preview-tile");
        card.dataset.width = next.width;
        const head = node("header", "db-tile-head"), heading = node("div");
        heading.append(node("h3", "db-tile-title", next.title), node("p", "db-tile-note", `${next.scopeLabel} · ${L(next.presentation)}`));
        head.append(heading);
        card.append(head, warnings(next.warnings));
        if (!next.rows.length) card.append(node("p", "db-sub", next.empty));
        for (const row of next.rows) card.append(makeRow(row, next, next.presentation === "detailed", false));
        if (next.moreCount) card.append(node("p", "db-sub", next.moreLabel));
        card.append(node("footer", "db-tile-footer", next.footer));
        target.replaceChildren(card);
      } catch {
        if (version === previewVersion && target.isConnected) target.replaceChildren(node("p", "db-sub", L("previewFailed")));
      } finally {
        if (version === previewVersion) target.removeAttribute("aria-busy");
      }
    }, 150);
  }
  function selectedRows(s) {
    if (active.row) return s.rows.filter((r) => r.id === active.row);
    const t = active.tile && tile(active.tile);
    if (!t) return s.rows;
    return s.rows.filter(
      (r) =>
        t.scope === "all" ||
        (t.scope === "attention" && r.attention) ||
        t.scope === `item:${r.id}` ||
        r.scopes.includes(t.scope),
    );
  }
  function fillDetails() {
    const box = q(".db-inspector"),
      s = source(active.source);
    if (!s) return;
    const rows = selectedRows(s);
    const selected = active.tile && model.tiles.find(t => t.id === active.tile);
    const signature = JSON.stringify([rows, s.warnings, s.message, s.trailing, selected?.empty]);
    if (active.signature === signature) return;
    active.signature = signature;
    text(
      box.querySelector("h3"),
      active.row ? rows[0]?.label || s.title : s.title,
    );
    text(box.querySelector(".db-inspector-head .db-sub"), s.trailing || "");
    const body = box.querySelector(".db-inspector-body");
    body.replaceChildren(warnings(s.warnings));
    if (s.message && rows.length) body.append(node("p", "db-detail-copy", s.message));
    const list = node("div", "db-detail-grid");
    for (const row of rows) {
      const item = node("div", "db-detail-resource"),
        head = node("div", "db-row");
      head.append(
        node("span", "", row.label),
        colored("span", "", row.value, row.valueColor),
      );
      item.append(head);
      if (row.explanation || row.detail)
        item.append(node("p", "db-detail-copy", row.explanation || row.detail));
      for (const m of [...(row.metrics || []), ...(row.details || [])]) {
        const field = node("div", "db-row");
        field.append(
          node("span", "db-muted", m.label),
          node("span", "", m.value ?? "—"),
        );
        item.append(field);
      }
      for (const v of row.volumes || []) {
        const field = node("div", "db-row");
        field.append(
          node("span", "", v.mount),
          colored("span", "", v.detail, v.tint),
        );
        item.append(field);
      }
      if (row.url) {
        const open = button(L("openRepo"), "openRepo", row.id);
        open.dataset.source = s.id;
        item.append(open);
      }
      if (!active.row) {
        const manage = button(L("manage"), "manage-resource", s.id);
        manage.dataset.scope = `item:${row.id}`;
        item.append(manage);
      }
      list.append(item);
    }
    if (!rows.length)
      list.append(node("p", "db-muted", selected?.empty || s.message || L("missingReading")));
    body.append(list);
  }
  function closeInspector() {
    clearTimeout(previewTimer);
    previewVersion++;
    active = null;
    q(".db-inspector").hidden = true;
    q(".db-inspector").replaceChildren();
  }
  async function save(next, message, undo = false) {
    if (busy) return false;
    if (!window.__TAURI__) {
      status(L("noSave"), true);
      return false;
    }
    busy = true;
    render(true);
    q(".db-inspector")
      .querySelectorAll("button,input,select")
      .forEach((el) => (el.disabled = true));
    const before = structuredClone(model.layout);
    try {
      const saved = await callRust("dashboard_save", {
        layout: next,
        expectedRevision: before.revision,
        width: root.clientWidth || innerWidth,
      });
      if (
        !saved?.layout?.tiles ||
        !Array.isArray(saved.sources) ||
        !Number.isSafeInteger(saved.layout.revision)
      )
        throw new Error(L("failed"));
      if (undo) history.pop();
      else {
        history.push(before);
        if (history.length > 20) history.shift();
      }
      model = saved;
      busy = false;
      closeInspector();
      render(true);
      status(message || L("saved"));
      return true;
    } catch (error) {
      busy = false;
      q(".db-inspector")
        .querySelectorAll("button,input,select")
        .forEach((el) => (el.disabled = false));
      await refresh();
      render(true);
      status(error?.message || String(error) || L("failed"), true);
      return false;
    }
  }
  function copyLayout() {
    return structuredClone(model.layout);
  }
  async function apply() {
    const form = q("#dashboardForm");
    if (!form || !form.reportValidity() || busy) return;
    const next = copyLayout(), draft = formTile();
    if (!draft) {
      status(L("missingScope"), true);
      return;
    }
    if (active.draft) next.tiles.push(draft);
    else Object.assign(next.tiles.find(t => t.id === draft.id), draft);
    await save(next);
  }
  function add(id, duplicate = false) {
    const original = duplicate ? tile(id) : null,
      s = source(original?.source || id);
    const newTile = original
      ? {
          ...original,
          id: crypto.randomUUID(),
          title: (original.title + L("duplicateSuffix")).slice(0, 64),
          hidden: false,
        }
      : { ...s.defaultTile, id: crypto.randomUUID() };
    openInspector({ kind: "configure", id: newTile.id, draft: newTile });
  }
  async function showOverview() {
    mode = "overview";
    legacy.removeAttribute("data-detail-source");
    overviewOpen = true;
    root.hidden = false;
    legacy.hidden = true;
    await refresh(true);
    render(true);
  }
  async function fullPanel(sourceId) {
    mode = "details";
    overviewOpen = false;
    if (sourceId) legacy.dataset.detailSource = sourceId;
    else legacy.removeAttribute("data-detail-source");
    root.hidden = true;
    legacy.hidden = false;
    for (const [source, id] of Object.entries(sourcePanels))
      document
        .getElementById(id)
        .toggleAttribute("data-detail-active", source === sourceId);
    await refreshCockpit();
    await refreshPanels();
    document.getElementById("dashboardBack").focus({ preventScroll: true });
  }
  root.addEventListener("click", async (event) => {
    const b = event.target.closest("button[data-action]");
    if (!b || !root.contains(b)) return;
    const action = b.dataset.action,
      id = b.dataset.id;
    if (action === "settings") {
      window.soladorSettings.open();
      return;
    }
    if (action === "close") {
      closeInspector();
      q('[data-action="edit"]').focus({ preventScroll: true });
      return;
    }
    if (action === "attention") {
      openInspector({ kind: "details", source: id });
      return;
    }
    if (action === "row") {
      openInspector({ kind: "details", source: b.dataset.source, row: id });
      return;
    }
    if (action === "details") {
      const t = tile(id);
      openInspector({ kind: "details", source: t.source, tile: id });
      return;
    }
    if (action === "allPanels") {
      await fullPanel(null);
      return;
    }
    if (action === "full") {
      await fullPanel(id);
      return;
    }
    if (action === "manage" || action === "manage-resource") {
      const draft = formTile();
      const sourceId = b.dataset.source || draft?.source || id;
      const scope = b.dataset.scope || draft?.scope || (active?.row ? `item:${active.row}` : (active?.tile && tile(active.tile)?.scope)) || "all";
      window.soladorSettings.open({ source: sourceId, scope });
      return;
    }
    if (action === "openRepo") {
      const r = source(b.dataset.source)?.rows.find((r) => r.id === id);
      if (r?.url) {
        try {
          await callRust("plugin:opener|open_url", { url: r.url });
        } catch (e) {
          status(String(e), true);
        }
      }
      return;
    }
    if (busy) return;
    if (action === "edit") {
      editing = !editing;
      closeInspector();
      render(true);
      return;
    }
    if (action === "catalog") {
      openInspector({ kind: "catalog" });
      return;
    }
    if (action === "configure") {
      editing = true;
      render(true);
      openInspector({ kind: "configure", id });
      return;
    }
    if (action === "apply") {
      await apply();
      return;
    }
    if (action === "add" || action === "duplicate") {
      await add(id, action === "duplicate");
      return;
    }
    if (action === "undo") {
      if (history.length)
        await save(
          structuredClone(history[history.length - 1]),
          L("saved"),
          true,
        );
      return;
    }
    const next = copyLayout(),
      t = next.tiles.find((t) => t.id === id);
    if (!t) return;
    if (action === "hide" || action === "restore") {
      t.hidden = action === "hide";
      await save(next);
      return;
    }
    if (action === "earlier" || action === "later") {
      const visible = next.tiles.filter((t) => !t.hidden),
        index = visible.indexOf(t),
        other = visible[index + (action === "earlier" ? -1 : 1)];
      if (!other) return;
      const a = next.tiles.indexOf(t),
        b = next.tiles.indexOf(other);
      [next.tiles[a], next.tiles[b]] = [next.tiles[b], next.tiles[a]];
      await save(next);
      root
        .querySelector(
          `[data-tile="${CSS.escape(id)}"] [data-action="configure"]`,
        )
        ?.focus({ preventScroll: true });
    }
  });
  root.addEventListener("submit", (e) => {
    if (e.target.id === "dashboardForm") {
      // WebKit can submit implicitly when Return commits a native picker.
      // Saving is an explicit Add/Apply button action, including keyboard use.
      e.preventDefault();
    }
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && pointerDrag) clearDrag();
  });
  root.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && active) {
      closeInspector();
      q('[data-action="edit"]').focus({ preventScroll: true });
    }
  });
  function clearDrag() {
    pointerDrag = null;
    dragged = null;
    root
      .querySelectorAll(".db-dragover")
      .forEach((el) => el.classList.remove("db-dragover"));
  }
  function dragTarget(e) {
    const el = document
      .elementFromPoint(e.clientX, e.clientY)
      ?.closest("[data-tile]");
    return el && root.contains(el) ? el : null;
  }
  // Pointer capture keeps the handle reliable in the native webview, whose
  // OS drag session does not consistently deliver HTML drag/drop events.
  root.addEventListener("pointerdown", (e) => {
    const handle = e.target.closest(".db-drag");
    if (!editing || busy || !handle || e.button !== 0 || !e.isPrimary) return;
    pointerDrag = {
      pointerId: e.pointerId,
      id: handle.closest("[data-tile]").dataset.tile,
      x: e.clientX,
      y: e.clientY,
    };
    handle.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  root.addEventListener("pointermove", (e) => {
    if (!pointerDrag || pointerDrag.pointerId !== e.pointerId) return;
    if (Math.hypot(e.clientX - pointerDrag.x, e.clientY - pointerDrag.y) < 5)
      return;
    dragged = pointerDrag.id;
    root
      .querySelectorAll(".db-dragover")
      .forEach((el) => el.classList.remove("db-dragover"));
    const el = dragTarget(e);
    if (el && el.dataset.tile !== dragged) el.classList.add("db-dragover");
  });
  root.addEventListener("pointerup", async (e) => {
    if (!pointerDrag || pointerDrag.pointerId !== e.pointerId) return;
    const from = dragged,
      to = dragTarget(e)?.dataset.tile;
    clearDrag();
    if (!from || !to || from === to || !editing || busy) return;
    const next = copyLayout(),
      fromIndex = next.tiles.findIndex((t) => t.id === from),
      toIndex = next.tiles.findIndex((t) => t.id === to);
    if (fromIndex < 0 || toIndex < 0) return;
    // Keep the destination's original position, so moving downward can reach
    // the end as well as moving upward can reach the beginning.
    const [moving] = next.tiles.splice(fromIndex, 1);
    next.tiles.splice(toIndex, 0, moving);
    await save(next);
  });
  root.addEventListener("pointercancel", clearDrag);
  root.addEventListener("lostpointercapture", clearDrag);
  document.addEventListener("solador:settings", (event) => {
    if (!model) return;
    if (event.detail) {
      root.hidden = true;
      legacy.hidden = true;
    } else {
      root.hidden = mode !== "overview";
      legacy.hidden = mode === "overview";
      refresh(true).then(() => {
        const draft = formTile();
        if (!draft) return;
        const select = q("#dashboard-scope"), s = source(draft.source);
        const choices = s.scopes.slice();
        if (!choices.some(c => c.value === draft.scope)) choices.push({value:draft.scope, label:L("missingScope")});
        select.replaceChildren(...choices.map(c => {
          const option = node("option", "", c.label);
          option.value = c.value;
          option.selected = c.value === draft.scope;
          return option;
        }));
        schedulePreview();
      });
    }
  });
  registerPanelRefresh(() => refresh(true));
  refresh(true);
  if (window.__TAURI__)
    setInterval(() => {
      if (mode === "overview" && !settingsOpen) refresh();
    }, 1000);
})();
