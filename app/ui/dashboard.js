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
    el.title = message;
    if (message) el.tabIndex = 0;
    else el.removeAttribute("tabindex");
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
    const items = node("div", "db-attention-items");
    items.tabIndex = 0;
    items.setAttribute("aria-label", L("attention"));
    attention.append(head, items);
    const editbar = node("div", "db-editbar");
    editbar.append(
      node("span", "", L("editHint")),
      button("", "hidden", null, "db-hidden-count"),
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
  function warnings(values, wrap = node("div", "db-warnings")) {
    const signature = JSON.stringify(values || []);
    if (wrap.warningSignature === signature) return wrap;
    wrap.warningSignature = signature;
    wrap.replaceChildren();
    wrap.removeAttribute("tabindex");
    wrap.removeAttribute("title");
    if (values?.length) {
      wrap.tabIndex = 0;
      wrap.setAttribute("role", "note");
      wrap.title = values.map(v => v.text).join(" · ");
    }
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
    b.append(name);
    if (row.counts?.length) {
      // A repo's `7 issues · 3 ready · 2 PRs`, on the row rather than under
      // it: the strip is the one line the tile has for it. Both halves are
      // Rust's — the value verbatim from the cell, the word already singular
      // or plural — and this only lays them side by side.
      // Each count is a fixed-width cell — the number right-aligned in
      // three characters, the word in a slot sized for that column's
      // longest word — so the numbers line up down the tile whether a row
      // reads `1 PR` or `10 PRs`. The header names the column for the CSS.
      const counts = node("span", "db-row-counts");
      b.classList.add("db-item-tabular");
      row.counts.forEach((c, i) => {
        if (i) counts.append(" · ");
        const cell = node("span", "db-count");
        cell.dataset.header = c.header;
        cell.append(node("strong", "", c.value), " ", node("span", "", c.label));
        counts.append(cell);
      });
      b.append(counts);
    }
    b.append(colored("span", "db-value", row.value, row.valueColor));
    b.title = [row.label, row.value, row.detail].filter(Boolean).join(" · ");
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
    if (detail || t.source === "sentryCrons") {
      const copy = node("p", "db-sub db-row-description", description || "");
      copy.title = description || "";
      wrap.append(copy);
    }
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
      el.dataset.source = t.source;
      el.dataset.presentation = t.presentation;
      el.dataset.scope = t.scopeLabel;
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
        content.tabIndex = 0;
        content.setAttribute("role", "region");
        content.setAttribute("aria-label", t.title);
        let warning = content.querySelector(":scope > .db-warnings");
        let rows = content.querySelector(":scope > .db-tile-rows");
        if (!warning) {
          warning = warnings(t.warnings);
          rows = node("div", "db-tile-rows");
          content.append(warning, rows);
        } else warnings(t.warnings, warning);
        rows.replaceChildren();
        if (!t.rows.length) {
          rows.append(node("p", "db-sub", t.empty));
          if (t.emptyAction) {
            const action = button(L(t.emptyAction === "configure" ? "editScope" : "manage"), t.emptyAction, t.id, "db-empty-action");
            action.dataset.source = t.source;
            action.dataset.scope = tile(t.id).scope;
            rows.append(action);
          }
        }
        for (const row of t.rows)
          rows.append(makeRow(row, t, t.presentation === "detailed"));
        if (t.moreCount) {
          const more = button(t.moreLabel, "details", t.id, "db-more db-plain");
          rows.append(more);
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
    for (const source of model.sources) {
      const item = model.attention.find(item => item.source === source.id);
      if (!item) {
        const slot = node("span", "db-attention-slot");
        slot.setAttribute("aria-hidden", "true");
        attention.append(slot);
        continue;
      }
      const b = button("", "attention", item.source), dot = node("span", "db-dot");
      dot.style.color = item.color;
      dot.setAttribute("aria-hidden", "true");
      b.title = item.label;
      b.append(dot, node("span", "db-attention-label", item.label));
      attention.append(b);
    }
    if (!model.attention.length)
      attention.append(node("span", "db-muted db-attention-quiet",
        model.sources.some(s => s.loading) ? L("loading") : L("quiet")));
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
      L("hiddenCount").replace("{count}", () => model.layout.tiles.filter((t) => t.hidden).length),
    );
    q(".db-hidden-count").disabled = busy;
    if (mode === "overview") updateTiles(force);
    if (force && active?.kind === "configure") {
      updatePlacementChoices();
      paintPlacement();
    }
    if (active?.kind === "details") fillDetails();
    if (active?.kind === "hidden") fillHidden();
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
    wrap.dataset.field = name;
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
    box.dataset.kind = next.kind;
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
      name.dataset.field = "title";
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
      const placement = selectField("position", L("position"), [], "");
      placement.classList.add("db-position-field");
      fields.append(placement);
      const actions = node("div", "db-form-actions");
      actions.append(
        button(L(next.draft ? "add" : "apply"), "apply", null, "db-primary"),
        button(L("manage"), "manage", t.source),
        node("span", "db-muted", L("scopeNote")),
      );
      if (!next.draft) actions.insertBefore(button(L("duplicate"), "duplicate", t.id), actions.children[1]);
      form.append(fields, actions);
      box.querySelector(".db-inspector-body").append(form);
      updatePlacementChoices(next.position || (next.draft ? "end" : "current"));
      const placementPreview = node("section", "db-placement-preview");
      placementPreview.setAttribute("aria-label", L("placementPreview"));
      placementPreview.append(
        node("h3", "", L("placementPreview")),
        node("p", "db-sub", L("placementHint")),
        node("ol", "db-placement-grid"),
        node("p", "db-placement-error"),
      );
      placementPreview.querySelector(".db-placement-error").setAttribute("role", "status");
      box.querySelector(".db-inspector-body").append(placementPreview);
      const preview = node("section", "db-preview");
      preview.setAttribute("aria-label", L("tilePreview"));
      preview.append(node("h3", "", L("tilePreview")), node("p", "db-sub", next.draft ? L("previewHint") : s.title), node("div", "db-preview-grid"));
      box.querySelector(".db-inspector-body").append(preview);
      if (!next.draft) {
        const removal = node("div", "db-removal");
        removal.append(button(L("remove"), "remove", t.id, "db-remove"), node("p", "db-sub", L("removeNote")));
        box.querySelector(".db-inspector-body").append(removal);
      }
      form.addEventListener("input", schedulePreview);
      schedulePreview();
      input.focus();
    } else if (next.kind === "catalog") {
      text(copy.children[0], L("catalog"));
      text(copy.children[1], L("catalogHint"));
      const body = box.querySelector(".db-inspector-body");
      const presets = node("section", "db-catalog-section"), choices = node("div", "db-catalog");
      presets.append(node("h3", "", L("presets")), node("p", "db-sub", L("presetsHint")), choices);
      for (const preset of model.presets || []) {
        const choose = button("", "preset", preset.id);
        choose.append(node("strong", "", preset.tile.title), node("span", "db-sub", preset.hint));
        choices.append(choose);
      }
      const all = node("section", "db-catalog-section"), catalog = node("div", "db-catalog");
      all.append(node("h3", "", L("allSources")), catalog);
      for (const s of model.sources) {
        const choose = button("", "add", s.id);
        choose.append(node("strong", "", s.title), node("span", "db-sub", s.hint));
        catalog.append(choose);
      }
      body.append(presets, all, button(L("hiddenTiles"), "hidden", null, "db-library-link"));
      choices.querySelector("button")?.focus({ preventScroll: true });
    } else if (next.kind === "hidden") {
      text(copy.children[0], L("hiddenTiles"));
      text(copy.children[1], L("hiddenHint"));
      const library = node("div", "db-hidden-library");
      const list = node("div", "db-hidden-list");
      list.setAttribute("aria-label", L("hiddenTiles"));
      library.append(list, node("div", "db-hidden-preview"));
      box.querySelector(".db-inspector-body").append(library);
      fillHidden();
      (list.querySelector('[aria-pressed="true"]') || box.querySelector('[data-action="close"]')).focus({ preventScroll: true });
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
    box.setAttribute("aria-label", copy.children[0].textContent);
  }
  function fillHidden() {
    const hidden = model.hiddenTiles || [];
    const list = q(".db-hidden-list"), target = q(".db-hidden-preview");
    if (!hidden.some(t => t.id === active.id)) active.id = hidden[0]?.id;
    const choices = hidden.map(t => [t.id, t.title, source(t.source).title, t.scopeLabel, t.width, t.presentation]);
    const signature = JSON.stringify([choices, active.id]);
    if (active.listSignature !== signature) {
      active.listSignature = signature;
      list.replaceChildren();
      for (const t of hidden) {
        const choose = button("", "hidden-preview", t.id);
        choose.setAttribute("aria-pressed", String(t.id === active.id));
        choose.append(node("strong", "", t.title), node("span", "db-sub", `${source(t.source).title} · ${t.scopeLabel}`), node("span", "db-sub", `${L(t.width)} · ${L(t.presentation)}`));
        list.append(choose);
      }
    }
    const selected = hidden.find(t => t.id === active.id);
    const previewSignature = JSON.stringify(selected);
    if (active.previewSignature === previewSignature && target.childElementCount) return;
    active.previewSignature = previewSignature;
    if (!selected) {
      target.replaceChildren(node("p", "db-sub", L("hiddenEmpty")));
      return;
    }
    // Refresh readings without replacing the Restore/Remove controls under focus.
    let grid = target.querySelector(".db-preview-grid");
    if (!grid || target.dataset.tileId !== selected.id) {
      target.dataset.tileId = selected.id;
      grid = node("div", "db-preview-grid");
      const actions = node("div", "db-form-actions");
      actions.append(actionLabel(button(L("restore"), "restore", selected.id, "db-primary"), selected.title), actionLabel(button(L("remove"), "remove", selected.id, "db-remove"), selected.title));
      target.replaceChildren(node("h3", "", L("tilePreview")), grid, actions, node("p", "db-sub db-removal-note", L("removeNote")));
    }
    grid.replaceChildren(previewCard(selected));
    target.querySelectorAll("button").forEach(b => {
      actionLabel(b, selected.title);
      b.disabled = busy;
    });
  }
  function previewCard(t) {
    const card = node("article", "db-preview-tile");
    card.dataset.width = t.width;
    card.dataset.source = t.source;
    card.dataset.presentation = t.presentation;
    card.dataset.scope = t.scopeLabel;
    const head = node("header", "db-tile-head"), heading = node("div");
    heading.append(node("h3", "db-tile-title", t.title), node("p", "db-tile-note", `${t.scopeLabel} · ${L(t.presentation)}`));
    head.append(heading);
    const content = node("div", "db-tile-content");
    content.tabIndex = 0;
    content.setAttribute("role", "region");
    content.setAttribute("aria-label", t.title);
    content.append(warnings(t.warnings));
    const rows = node("div", "db-tile-rows");
    if (!t.rows.length) rows.append(node("p", "db-sub", t.empty));
    for (const row of t.rows) rows.append(makeRow(row, t, t.presentation === "detailed", false));
    if (t.moreCount) rows.append(node("p", "db-sub", t.moreLabel));
    content.append(rows);
    card.append(head, content);
    card.append(node("footer", "db-tile-footer", t.footer));
    return card;
  }
  function formTile() {
    const form = q("#dashboardForm");
    if (!form || active?.kind !== "configure") return null;
    const original = active.draft || tile(active.id);
    if (!original) return null;
    const fields = new FormData(form);
    return { ...original, title: String(fields.get("title")).trim(), scope: fields.get("scope"), presentation: fields.get("presentation"), width: fields.get("width") };
  }
  function updatePlacementChoices(selected = q("#dashboard-position")?.value) {
    const select = q("#dashboard-position");
    if (!select) return;
    const choices = active.draft ? [] : [{ value: "current", label: L("positionCurrent") }];
    choices.push({ value: "start", label: L("positionStart") }, { value: "end", label: L("positionEnd") });
    for (const t of model.layout.tiles.filter(t => !t.hidden && t.id !== active.id))
      choices.push({ value: `after:${t.id}`, label: L("positionAfter").replace("{title}", () => t.title) });
    if (!choices.some(choice => choice.value === selected))
      choices.push({ value: selected, label: L("positionMissing") });
    // Polls must not disturb a native picker while it is open.
    const signature = JSON.stringify(choices);
    if (select.dataset.choices === signature) return;
    select.dataset.choices = signature;
    select.replaceChildren(...choices.map(choice => {
      const option = node("option", "", choice.label);
      option.value = choice.value;
      option.selected = choice.value === selected;
      return option;
    }));
  }
  function placedLayout(draft) {
    const next = copyLayout(), position = q("#dashboard-position").value;
    const original = next.tiles.findIndex(t => t.id === draft.id);
    if (position === "current" && original >= 0) {
      next.tiles[original] = draft;
      return next;
    }
    next.tiles = next.tiles.filter(t => t.id !== draft.id);
    let index = position === "start" ? 0 : next.tiles.length;
    if (position.startsWith("after:")) {
      const anchor = next.tiles.findIndex(t => t.id === position.slice(6) && !t.hidden);
      if (anchor < 0) throw new Error(L("positionUnavailable"));
      index = anchor + 1;
    } else if (position !== "start" && position !== "end") {
      throw new Error(L("positionUnavailable"));
    }
    next.tiles.splice(index, 0, draft);
    return next;
  }
  function paintPlacement() {
    const grid = q(".db-placement-grid"), draft = formTile();
    if (!grid || !draft) return;
    const select = q("#dashboard-position"), error = q(".db-placement-error");
    try {
      const proposed = placedLayout(draft);
      select.setCustomValidity("");
      error.textContent = "";
      grid.replaceChildren(...proposed.tiles.filter(t => !t.hidden).map(t => {
        const item = node("li", "db-placement-tile");
        item.dataset.placementTile = t.id;
        item.dataset.width = t.width;
        if (t.id === draft.id) item.setAttribute("aria-current", "true");
        item.append(node("strong", "db-placement-title", t.title || source(t.source).title), node("span", "db-sub", L(t.width)));
        return item;
      }));
    } catch (e) {
      grid.replaceChildren();
      error.textContent = e.message;
      select.setCustomValidity(e.message);
    }
  }
  function schedulePreview() {
    paintPlacement();
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
        target.replaceChildren(previewCard(next));
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
    body.tabIndex = 0;
    body.setAttribute("role", "region");
    body.setAttribute("aria-label", s.title);
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
      for (const m of [
        ...(row.metrics || []),
        ...(row.counts || []),
        ...(row.details || []),
      ]) {
        const field = node("div", "db-row");
        // A count carries the table header it sits under, so the eight
        // numbers list in one vocabulary here rather than three words and
        // five headers.
        field.append(
          node("span", "db-muted", m.header ?? m.label),
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
    const draft = formTile();
    if (!draft) {
      status(L("missingScope"), true);
      return;
    }
    let next;
    try { next = placedLayout(draft); }
    catch (e) { status(e.message, true); return; }
    if (await save(next)) {
      const savedTile = tiles.get(draft.id);
      savedTile?.scrollIntoView({ block: "nearest" });
      savedTile?.querySelector('[data-action="configure"]')?.focus({ preventScroll: true });
    }
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
    openInspector({ kind: "configure", id: newTile.id, draft: newTile, position: original ? `after:${original.id}` : "end" });
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
    if (action === "hidden") {
      openInspector({ kind: "hidden" });
      return;
    }
    if (action === "hidden-preview") {
      active.id = id;
      render();
      return;
    }
    if (action === "preset") {
      const preset = model.presets?.find(p => p.id === id);
      if (preset) {
        const draft = { ...preset.tile, id: crypto.randomUUID() };
        openInspector({ kind: "configure", id: draft.id, draft, position: "end" });
      }
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
    if (action === "remove") {
      const fromLibrary = active?.kind === "hidden";
      next.tiles = next.tiles.filter(t => t.id !== id);
      if (await save(next, L("removed"))) {
        if (fromLibrary) openInspector({ kind: "hidden" });
        else {
          q(".db-chrome").scrollIntoView({ block: "start" });
          q('[data-action="undo"]').focus({ preventScroll: true });
        }
      }
      return;
    }
    if (action === "hide" || action === "restore") {
      t.hidden = action === "hide";
      if (await save(next, L(t.hidden ? "hideSaved" : "restored"))) {
        if (t.hidden) {
          q(".db-chrome").scrollIntoView({ block: "start" });
          q(".db-hidden-count").focus({ preventScroll: true });
        }
        else {
          tiles.get(id)?.scrollIntoView({ block: "nearest" });
          tiles.get(id)?.querySelector('[data-action="configure"]')?.focus({ preventScroll: true });
        }
      }
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
