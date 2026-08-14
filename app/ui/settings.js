// The Settings surface. Same discipline as the cockpit: every string here
// arrives from Rust (`app/src-tauri/src/settings.rs`) and this file does
// layout, wiring, and nothing else. A label typed into this file is a label
// that can drift from the original app without a test noticing.
//
// Nothing below uses `innerHTML`: the whole surface is built with
// createElement + textContent, so host names, mount paths and repo slugs (all
// of which come from a remote agent or from user input) reach the DOM as text
// and cannot reach it as markup. That is stronger than escaping, and it is why
// `esc()` appears nowhere in this file.
//
// It also adds no ACL surface. Every command it calls is app-defined, which
// Tauri's ACL permits without a grant, so `capabilities/default.json` keeps
// its empty `permissions` list -- the reason this is an in-app view and not a
// second window.
//
// Wrapped in an IIFE, and that is not decoration: classic scripts share one
// global scope, so a top-level `function render()` here would silently REPLACE
// app.js's `render()` and every host card would stop painting. (It did.) The
// only names crossing the boundary are the three app.js exposes -- `callRust`,
// `settingsOpen`, `refreshCockpit` -- plus the one test hook at the bottom.
(function () {

const S = {
  /** The last `settings_view` payload. Null until Settings is first opened. */
  view: null,
  tab: "general",
  status: "",
  /** Host id -> its last Test result line. Survives a re-render. */
  tests: new Map(),
  /** Which Layout breakpoint the editor is showing, by its `minWidth`. `null`
   *  means "the first one" — see `selectedBand`. */
  band: null,
  /** The last `settings_probe_status_vendor` answer, or `null` before anything
   *  has been probed. Held here like `tests` is: which host you last probed is
   *  a *view* state, not something the store remembers.
   *
   *  Rust's shape, unchanged — `{baseUrl, components, reason}`, where
   *  `components` is a non-empty list or `null` and `reason` is the finding or
   *  `null`. This file never fills either in. */
  probe: null,
};

const $s = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

function button(label, cls) {
  const b = node("button", "btn " + cls, label);
  b.type = "button";
  return b;
}

/** A labelled control. Real `<label for>`, so the form is navigable and the
 *  Playwright suite can address a field by the name Rust gave it. */
function field(id, labelText, input) {
  const wrap = node("div", "field");
  const label = node("label", "lbl", labelText);
  label.htmlFor = id;
  input.id = id;
  wrap.append(label, input);
  return wrap;
}

function textInput(value, type = "text") {
  const input = node("input", "input");
  input.type = type;
  const initial = value === undefined || value === null ? "" : String(value);
  input.value = initial;
  // Also the default, so `value !== defaultValue` means "typed and not yet
  // applied" — the one distinction `unappliedEdits` runs on.
  input.defaultValue = initial;
  return input;
}

function numberInput(value, min, max) {
  const input = textInput(value, "number");
  if (min !== undefined) input.min = String(min);
  if (max !== undefined) input.max = String(max);
  return input;
}

function select(options, value) {
  const sel = node("select", "input");
  for (const option of options) {
    const opt = node("option", null, option.label);
    opt.value = String(option.value);
    sel.appendChild(opt);
  }
  sel.value = String(value);
  return sel;
}

function checkbox(checked) {
  const input = node("input", "toggle");
  input.type = "checkbox";
  input.checked = !!checked;
  return input;
}

function help(text) {
  return node("p", "help", text);
}

/** A one-button action row. */
function actionRow(...controls) {
  const row = node("div", "row");
  row.append(...controls);
  return row;
}

function group(heading) {
  const box = node("section", "group");
  if (heading) box.appendChild(node("h2", "group-hdr", heading));
  return box;
}

/** Whole numbers only: `refreshIntervalSecs`, `coreRowSpan` and the Sentry
 *  quota are integers in Rust, and a NaN is not even JSON. */
const int = (raw) => Math.max(0, Math.round(Number(raw) || 0));

// MARK: talking to Rust

/** Typed-but-unapplied edits, as `[id, value]` pairs.
 *
 *  Every Apply-gated field would otherwise be silently wiped by ANY other
 *  mutation's re-render — the classic being an org ID typed and then lost to
 *  the adjacent credential's Save, while the status line says "Saved.".
 *  Passwords are excluded on principle: a credential is cleared the moment it
 *  is handed to Rust, and nothing may carry one across a render. */
function unappliedEdits() {
  const edits = [];
  for (const el of document.querySelectorAll("#settings input[id]")) {
    if (el.type === "password" || el.type === "checkbox") continue;
    if (el.value !== el.defaultValue) edits.push([el.id, el.value]);
  }
  return edits;
}

/** Puts unapplied edits back into the freshly rendered fields. The `input`
 *  event re-runs listeners that derive state from the value (e.g. an Add
 *  button's disabled-ness) — but never `change`, which is what mutations hang
 *  off, and a restore must not save anything. */
function restoreEdits(edits) {
  for (const [id, value] of edits) {
    const el = document.getElementById(id);
    if (!el || el.type === "password") continue;
    el.value = value;
    el.dispatchEvent(new Event("input"));
  }
}

/** Applies a mutation's `{status, settings}` answer. The frontend never
 *  patches its own copy -- it re-renders from what was actually persisted, so
 *  it cannot show an edit that failed to save. Edits not yet handed to Rust
 *  are the one exception: they ride across the render (`unappliedEdits`),
 *  because losing them is not truth, it is data loss -- a submitted field is
 *  cleared by its own handler and so never rides. */
async function apply(result) {
  if (!result) return;
  const edits = unappliedEdits();
  S.view = result.settings;
  S.status = result.status || "";
  render();
  restoreEdits(edits);
}

async function mutate(command, args) {
  apply(await callRust(command, args));
}

// MARK: rendering

function renderTabs() {
  const bar = $s("settingsTabs");
  bar.replaceChildren();
  for (const tab of S.view.tabs) {
    const b = button(tab.title, "tab");
    b.dataset.tab = tab.id;
    if (tab.id === S.tab) b.dataset.active = "true";
    b.addEventListener("click", () => {
      S.tab = tab.id;
      render();
    });
    bar.appendChild(b);
  }
}

/** One credential: a password field, Save, Clear, and a badge that says
 *  whether something is stored -- never what. The value is cleared from the
 *  field the moment it is handed to Rust, and no payload ever brings one
 *  back, so a stored credential has no path into the DOM. */
function secretControls(box, secret) {
  box.dataset.secret = secret.key;

  const input = textInput("", "password");
  box.appendChild(field(`secret-${secret.key}`, secret.fieldLabel, input));

  const row = node("div", "row");
  const save = button(secret.saveLabel, "save");
  const clear = button(secret.clearLabel, "clear");
  save.disabled = true;
  clear.disabled = !secret.stored;
  input.addEventListener("input", () => {
    save.disabled = input.value.length === 0;
  });
  save.addEventListener("click", () => {
    const value = input.value;
    input.value = "";
    save.disabled = true;
    mutate("settings_save_secret", { key: secret.key, value });
  });
  clear.addEventListener("click", () => mutate("settings_clear_secret", { key: secret.key }));
  row.append(save, clear, node("span", "grow"));
  if (secret.stored) row.appendChild(node("span", "badge-ok", secret.storedLabel));
  box.append(row, help(secret.help));
  return box;
}

/** A group that holds nothing but one credential. */
function secretGroup(heading, secret) {
  return secretControls(group(heading), secret);
}

function generalTab(g) {
  const box = group(g.heading);

  const interval = select(g.refreshInterval.options, g.refreshInterval.value);
  box.append(field("general-interval", g.refreshInterval.label, interval), help(g.refreshInterval.help));

  const span = numberInput(g.coreRowSpan.value, g.coreRowSpan.min, g.coreRowSpan.max);
  box.append(field("general-core-rows", g.coreRowSpan.label, span), help(g.coreRowSpan.help));

  // No host-overflow picker here: it is per breakpoint now (the Layout tab),
  // because one global switch could not say "tabs in a narrow column, side by
  // side when wide".
  const apply = button(g.saveLabel, "apply");
  apply.addEventListener("click", () =>
    mutate("settings_save_general", {
      refreshIntervalSecs: int(interval.value),
      coreRowSpan: int(span.value),
    })
  );
  box.appendChild(actionRow(apply));
  return [box];
}

/** One panel's row in the Layout editor: its title, a width picker, and the
 *  two buttons that walk it along the order.
 *
 *  Every control saves immediately, like the Portfolio tab's checkbox — there
 *  is no Apply here. The re-render then comes from what Rust persisted, so the
 *  list can never show an order that failed to save, and `canMoveUp` /
 *  `canMoveDown` are Rust's answers rather than an index comparison here.
 *
 *  Every mutation names the breakpoint it edits by `minWidth`, never by index:
 *  adding one re-sorts the list, and an index would then address the wrong
 *  band. */
function layoutRow(t, band, panel) {
  const row = node("div", "layout-row");
  row.dataset.panel = panel.id;
  row.append(node("span", "layout-name", panel.title), node("span", "grow"));

  const span = select(t.spanOptions, panel.span);
  span.addEventListener("change", () =>
    mutate("settings_set_panel_span", {
      minWidth: band.minWidth,
      panel: panel.id,
      span: span.value,
    })
  );
  row.appendChild(field(`layout-span-${panel.id}`, t.spanLabel, span));

  for (const [label, direction, enabled] of [
    [t.upLabel, "up", panel.canMoveUp],
    [t.downLabel, "down", panel.canMoveDown],
  ]) {
    const move = button(label, "move");
    move.dataset.direction = direction;
    move.disabled = !enabled;
    move.addEventListener("click", () =>
      mutate("settings_move_panel", { minWidth: band.minWidth, panel: panel.id, direction })
    );
    row.appendChild(move);
  }
  return row;
}

/** The rows this order packs into, drawn at their real proportions.
 *
 *  The packing is Rust's (`CockpitLayout::from_order`) and arrives as rows of
 *  cells carrying the `weight` — the quarters — each panel gets. Re-deriving
 *  "what will this look like" from the spans here would be a second
 *  implementation of the packer, free to promise an arrangement the cockpit
 *  then does not render.
 *
 *  Placed on the same four-quarter grid `applyPanelRows` uses, for the same
 *  reason: a per-row track list sized by that row's own weights draws a half
 *  beside two quarters narrower than a half beside one half, so the preview
 *  would promise proportions the cockpit does not paint. */
function layoutPreview(preview) {
  const box = group(preview.label);
  for (const row of preview.rows) {
    const line = node("div", "layout-preview-row");
    let start = 1;
    for (const cell of row) {
      const weight = Math.min(4, Math.max(1, Number(cell.weight) | 0));
      const tile = node("div", "layout-tile");
      // CSSOM, never a `style=""` attribute -- `style-src 'self'`. Same setter
      // and the same spans app.js puts on a real panel.
      tile.style.gridColumn = `${start} / span ${weight}`;
      start += weight;
      tile.append(node("span", "layout-tile-name", cell.title));
      tile.append(node("span", "layout-tile-span", cell.spanLabel));
      line.appendChild(tile);
    }
    box.appendChild(line);
  }
  return box;
}

/** The band the editor is showing.
 *
 *  Held here rather than in the payload because it is a *view* state: which
 *  arrangement you are editing is not something the cockpit renders or the
 *  store remembers. A selection that no longer exists (its band was removed,
 *  or Reset collapsed them all) falls back to the first — never to nothing. */
function selectedBand(t) {
  return t.breakpoints.find((b) => b.minWidth === S.band) || t.breakpoints[0];
}

/** The breakpoint switcher: one button per band, plus the width form that adds
 *  another. Same `.tab` control the Settings tab bar uses, so the two read as
 *  the same kind of choice. */
function breakpointBar(t, current) {
  const box = group(null);
  const bar = node("div", "tabs");
  for (const band of t.breakpoints) {
    const b = button(band.label, "tab");
    b.dataset.band = String(band.minWidth);
    if (band.minWidth === current.minWidth) b.dataset.active = "true";
    b.addEventListener("click", () => {
      S.band = band.minWidth;
      render();
    });
    bar.appendChild(b);
  }
  box.appendChild(bar);

  const width = numberInput("", 0);
  const add = button(t.add.buttonLabel, "add");
  const submit = () => {
    const value = int(width.value);
    // Reset at submit, like the Add Host form: a submitted field must not ride
    // the re-render as an unapplied edit.
    width.value = "";
    // Select what is about to be created, so the new band is what the editor
    // shows when Rust answers.
    S.band = value;
    mutate("settings_add_breakpoint", { minWidth: value });
  };
  add.addEventListener("click", submit);
  width.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submit();
  });
  box.append(field("layout-add-width", t.add.widthLabel, width), actionRow(add), help(t.add.help));
  return box;
}

function layoutTab(t) {
  const current = selectedBand(t);
  const list = group(t.heading);
  list.appendChild(help(t.help));

  const overflow = select(t.overflowOptions, current.hostOverflow);
  overflow.addEventListener("change", () =>
    mutate("settings_set_breakpoint_overflow", {
      minWidth: current.minWidth,
      hostOverflowMode: overflow.value,
    })
  );
  list.append(field("layout-overflow", t.overflowLabel, overflow), help(t.overflowHelp));

  for (const panel of current.rows) list.appendChild(layoutRow(t, current, panel));

  const remove = button(t.removeLabel, "delete");
  // The last band standing cannot go — Rust's `canRemove`, not a length
  // compared here.
  remove.disabled = !current.canRemove;
  remove.addEventListener("click", () => {
    S.band = null;
    mutate("settings_remove_breakpoint", { minWidth: current.minWidth });
  });

  const reset = group(null);
  const button_ = button(t.resetLabel, "delete");
  // A store that has never carried a layout has nothing to reset. Whether that
  // is the case is Rust's `isDefault`, not "do these rows look default to me".
  button_.disabled = t.isDefault;
  button_.addEventListener("click", () => {
    S.band = null;
    mutate("settings_reset_layout", {});
  });
  reset.append(actionRow(button_), help(t.resetHelp));

  return [
    breakpointBar(t, current),
    list,
    layoutPreview(current.preview),
    actionRow(remove),
    reset,
  ];
}

/** One "hidden volume" line: the mount, and the button that unhides it. */
function hiddenRow(t, mount, hostId) {
  const row = node("div", "hidden-row");
  row.append(node("span", "mount", mount));
  row.appendChild(node("span", "grow"));
  const unhide = button(t.unhideLabel, "unhide");
  unhide.addEventListener("click", () =>
    mutate("settings_unhide_volume", { hostId: hostId, mount })
  );
  row.appendChild(unhide);
  return row;
}

function hostsTab(t) {
  const list = group(t.heading);
  if (t.rows.length === 0) {
    list.appendChild(help(t.empty));
  }
  for (const host of t.rows) {
    const row = node("div", "host-row");
    row.dataset.host = host.id;

    const head = node("div", "row");
    const names = node("div", "stack");
    names.append(node("span", "host-name", host.name), node("span", "dim", host.endpoint));
    const result = node("span", "result", S.tests.get(host.id) || "");
    names.appendChild(result);
    head.append(names, node("span", "grow"));
    head.appendChild(
      node("span", host.tokenStored ? "badge-ok" : "badge-dim",
        host.tokenStored ? t.tokenStoredLabel : t.noTokenLabel)
    );

    const test = button(t.testLabel, "test");
    test.addEventListener("click", async () => {
      result.textContent = t.testingLabel;
      const answer = await callRust("settings_test_host", { id: host.id });
      if (!answer) return;
      S.tests.set(answer.id, answer.result);
      result.textContent = answer.result;
    });

    const enabled = checkbox(host.enabled);
    enabled.addEventListener("change", () =>
      mutate("settings_set_host_enabled", { id: host.id, enabled: enabled.checked })
    );

    const remove = button(t.deleteLabel, "delete");
    remove.addEventListener("click", () => {
      S.tests.delete(host.id);
      mutate("settings_remove_host", { id: host.id });
    });

    head.append(test, enabled, remove);
    row.appendChild(head);
    for (const mount of host.hiddenVolumes) row.appendChild(hiddenRow(t, mount, host.id));
    list.appendChild(row);
  }

  const boxes = [list];

  // Only when it has entries: this shell has no local collector, so an empty
  // section here would be a heading for something that cannot exist yet.
  if (t.localHidden.mounts.length > 0) {
    const local = group(t.localHidden.heading);
    for (const mount of t.localHidden.mounts) local.appendChild(hiddenRow(t, mount, null));
    boxes.push(local);
  }

  const add = group(t.add.heading);
  const name = textInput("");
  const address = textInput("");
  const port = textInput(t.add.portDefault);
  const token = textInput("", "password");
  add.append(
    field("host-name", t.add.nameLabel, name),
    field("host-address", t.add.addressLabel, address),
    field("host-port", t.add.portLabel, port),
    field("host-token", t.add.tokenLabel, token)
  );

  const submit = button(t.add.buttonLabel, "add");
  const syncAdd = () => {
    submit.disabled = name.value.trim() === "" || address.value.trim() === "";
  };
  name.addEventListener("input", syncAdd);
  address.addEventListener("input", syncAdd);
  syncAdd();
  submit.addEventListener("click", () => {
    const args = {
      name: name.value,
      address: address.value,
      port: port.value,
      token: token.value,
    };
    // Dropped before the round-trip, not after: the token has no reason to
    // outlive the call, and a rejected save must not leave it sitting in the
    // DOM. The other three reset so the form comes back empty rather than
    // riding the re-render as unapplied edits.
    token.value = "";
    name.value = "";
    address.value = "";
    port.value = t.add.portDefault;
    mutate("settings_add_host", args);
  });
  add.append(actionRow(submit), help(t.add.help));
  boxes.push(add);
  boxes.push(rulesGroup(t.rules));
  return boxes;
}

/**
 * One container group rule.
 *
 * Every control writes ONE field, through `settings_set_container_rule`, and
 * the surface then re-renders from the `{status, settings}` it gets back. That
 * is the port of the original's re-read-on-access bindings: this file never assembles
 * a whole rule out of what its own inputs happen to hold, so editing the label
 * cannot clobber a pattern that changed a moment earlier. The row is addressed
 * by its index in the persisted list, which is also the order the rule engine
 * matches in.
 */
function ruleRow(t, rule) {
  const row = node("div", "rule-row");
  row.dataset.rule = String(rule.index);

  const set = (field, value) =>
    mutate("settings_set_container_rule", { index: rule.index, field, value });

  const action = select(t.actions, rule.action);
  action.title = t.actionLabel;
  action.addEventListener("change", () => set("action", action.value));

  const pattern = textInput(rule.pattern);
  pattern.placeholder = t.patternPrompt;
  pattern.title = t.patternLabel;
  pattern.autocapitalize = "off";
  pattern.spellcheck = false;
  // On change, not on every keystroke: each save re-renders the list, and a
  // per-keystroke write would rebuild the field under the caret and eat the
  // rest of the word. Same reason the watched-workflows field uses `change`.
  pattern.addEventListener("change", () => set("pattern", pattern.value));

  const controls = [action, pattern];

  // Only a Collapse rule has an aggregate to name or count — and whether that
  // is so is Rust's `collapseOnly`, not a comparison against a literal action
  // string typed here.
  if (rule.collapseOnly) {
    const label = textInput(rule.label);
    label.placeholder = t.labelPrompt;
    label.title = t.labelLabel;
    label.addEventListener("change", () => set("label", label.value));

    const expected = textInput(rule.expected);
    expected.placeholder = t.expectedPrompt;
    expected.title = t.expectedLabel;
    expected.className = "input rule-expected";
    // Deliberately a text field, not `type="number"`: the empty state means
    // "no expectation", and a number input in some browsers reports an
    // unparseable entry as "" — which would silently clear an expectation the
    // operator was mid-way through typing. Rust decides what the string means
    // (`parse_expected_count`), including that 0 and nonsense clear it.
    expected.addEventListener("change", () => set("expected", expected.value));

    controls.push(node("span", "rule-arrow", t.arrow), label, expected);
  }

  const host = select(rule.hostOptions, rule.host);
  host.title = t.hostLabel;
  host.className = "input rule-host";
  host.addEventListener("change", () => set("host", host.value));
  controls.push(host, node("span", "grow"));

  const remove = button(t.deleteLabel, "delete");
  remove.addEventListener("click", () =>
    mutate("settings_remove_container_rule", { index: rule.index })
  );
  controls.push(remove);

  row.append(...controls);
  return row;
}

/** The Container Group Rules editor, under the host list it scopes rules to. */
function rulesGroup(t) {
  const box = group(t.heading);
  for (const rule of t.rows) box.appendChild(ruleRow(t, rule));

  const add = button(t.addLabel, "add-rule");
  add.addEventListener("click", () => mutate("settings_add_container_rule", {}));
  box.append(actionRow(add), help(t.help));
  return box;
}

function portfolioTab(t) {
  const list = group(t.heading);
  if (t.rows.length === 0) list.appendChild(help(t.empty));
  for (const repo of t.rows) {
    const row = node("div", "repo-row");
    row.dataset.repo = repo.slug;

    const head = node("div", "row");
    head.append(node("span", "slug", repo.slug));
    // Rust's sentence for a repo naming an account that no longer exists.
    // Shown beside the slug rather than folded into the picker: the picker's
    // "Unattributed" is a state the operator chose, and this one is not.
    if (repo.accountMissing) head.appendChild(node("span", "badge-dim", t.missingAccountLabel));
    head.appendChild(node("span", "grow"));

    const enabled = checkbox(repo.enabled);
    enabled.addEventListener("change", () =>
      mutate("settings_set_repo_enabled", { slug: repo.slug, enabled: enabled.checked })
    );
    const remove = button(t.deleteLabel, "delete");
    remove.addEventListener("click", () => mutate("settings_remove_repo", { slug: repo.slug }));
    head.append(enabled, remove);

    const workflows = textInput(repo.workflows);
    workflows.disabled = !repo.enabled;
    // On change, not on every keystroke: a per-keystroke save would re-render
    // the list under the caret and eat the rest of the word.
    workflows.addEventListener("change", () =>
      mutate("settings_set_repo_workflows", { slug: repo.slug, workflows: workflows.value })
    );
    row.append(head, field(`repo-workflows-${repo.slug}`, t.workflowsLabel, workflows));

    // Which account fetches this repo. Rust sends no options at all in a store
    // with no accounts, and this file renders no control rather than deciding
    // for itself when one is worth showing. `accountId` is null for an
    // unattributed repo, which is the picker's own first option -- never the
    // first account, which is the guess the whole change exists to refuse.
    if (t.accountOptions.length > 0) {
      const account = select(t.accountOptions, repo.accountId || "");
      account.addEventListener("change", () =>
        // Empty back to null: the option's value is a string, and "" is the
        // unattributed state rather than an id of no characters.
        mutate("settings_set_repo_account", {
          slug: repo.slug,
          accountId: account.value || null,
        })
      );
      row.appendChild(field(`repo-account-${repo.slug}`, t.accountLabel, account));
    }
    list.appendChild(row);
  }

  const add = group(t.add.heading);
  const slug = textInput("");
  add.appendChild(field("repo-slug", t.add.slugLabel, slug));
  const submit = button(t.add.buttonLabel, "add");
  // The original disables Add until the slug at least looks like `owner/name`; Rust
  // re-checks it (and rejects duplicates) either way -- this is a hint, not
  // the validation.
  const syncAdd = () => {
    submit.disabled = !slug.value.includes("/");
  };
  slug.addEventListener("input", syncAdd);
  syncAdd();
  const submitSlug = () => {
    const value = slug.value;
    // Reset at submit, like the Add Host form: a submitted field must not
    // ride the re-render as an unapplied edit.
    slug.value = "";
    mutate("settings_add_repo", { slug: value });
  };
  submit.addEventListener("click", submitSlug);
  slug.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !submit.disabled) submitSlug();
  });
  add.append(actionRow(submit), help(t.add.help));
  return [list, add];
}

/**
 * One vendor account: what it fetches, whether a token is stored, and the
 * controls that rename, pause or remove it.
 *
 * Delete is two-step exactly when Rust sent a `removePrompt` — that is, when
 * tracked repos depend on this account. The prompt is Rust's sentence, shown
 * verbatim; this file decides only *when* it appears, never what it says. An
 * account nothing depends on deletes in one click like a host does: a
 * confirmation over no consequence is what teaches an operator to click through
 * the one that has one.
 */
function accountRow(t, account) {
  const row = node("div", "account-row");
  row.dataset.account = account.id;

  const head = node("div", "row");
  const names = node("div", "stack");
  names.append(node("span", "host-name", account.label), node("span", "dim", account.vendorLabel));

  // What this account fetches, on the row that removes it. Two nodes rather
  // than one string: a template literal here is this file writing a sentence
  // Rust did not.
  const repos = node("div", "row");
  if (account.repos.length === 0) {
    repos.appendChild(node("span", "result", t.noReposLabel));
  } else {
    repos.append(node("span", "result", t.reposLabel), node("span", "result", account.repos.join(", ")));
  }
  names.appendChild(repos);
  head.append(names, node("span", "grow"));
  head.appendChild(
    node("span", account.stored ? "badge-ok" : "badge-dim",
      account.stored ? t.tokenStoredLabel : t.noTokenLabel)
  );

  const enabled = checkbox(account.enabled);
  enabled.addEventListener("change", () =>
    mutate("settings_set_account_enabled", { id: account.id, enabled: enabled.checked })
  );

  // Built before the button that reveals it, and empty until then.
  const confirm = node("div", "stack");
  confirm.dataset.confirm = "remove";
  confirm.hidden = true;

  const remove = button(t.deleteLabel, "delete");
  const removeNow = () => mutate("settings_remove_account", { id: account.id });
  remove.addEventListener("click", () => {
    if (!account.removePrompt) {
      removeNow();
      return;
    }
    remove.disabled = true;
    confirm.hidden = false;
  });

  const proceed = button(t.deleteLabel, "delete");
  proceed.addEventListener("click", removeNow);
  const cancel = button(t.cancelLabel, "cancel");
  cancel.addEventListener("click", () => {
    confirm.hidden = true;
    remove.disabled = false;
  });
  confirm.append(node("p", "confirm", account.removePrompt || ""), actionRow(proceed, cancel));

  head.append(enabled, remove);

  // The name and the token travel together through `settings_save_account`,
  // which is one command for create and update alike. An empty token field
  // leaves the stored credential alone -- that is Rust's rule, and this file
  // does not re-state it by disabling anything.
  const name = textInput(account.label);
  const token = textInput("", "password");
  const save = button(t.saveLabel, "save");
  save.addEventListener("click", () => {
    const args = {
      id: account.id,
      vendor: account.vendor,
      label: name.value,
      token: token.value,
    };
    // Dropped before the round-trip, like the Add Host form: the token has no
    // reason to outlive the call, and a rejected save must not leave it in the
    // DOM.
    token.value = "";
    mutate("settings_save_account", args);
  });

  row.append(
    head,
    confirm,
    field(`account-name-${account.id}`, t.nameLabel, name),
    field(`account-token-${account.id}`, t.tokenLabel, token),
    actionRow(save)
  );
  return row;
}

/**
 * The Accounts tab: one credential per account, not per vendor.
 *
 * Every row carries the repos attributed to it, so what removing an account
 * costs is on screen before the button is pressed. Nothing here re-homes an
 * orphaned repo onto a surviving account — that would be this file inventing an
 * owner, and Rust refuses to do it for the same reason.
 */
function accountsTab(t) {
  const list = group(t.heading);
  if (t.rows.length === 0) list.appendChild(help(t.empty));
  for (const account of t.rows) list.appendChild(accountRow(t, account));
  list.appendChild(help(t.help));

  const add = group(t.add.heading);
  const vendor = select(t.add.vendorOptions, t.add.vendorOptions[0].value);
  const name = textInput("");
  const token = textInput("", "password");
  add.append(
    field("account-vendor", t.add.vendorLabel, vendor),
    field("account-name", t.add.nameLabel, name),
    field("account-token", t.add.tokenLabel, token)
  );

  const submit = button(t.add.buttonLabel, "add");
  // A hint, not the validation: Rust re-checks the name and the vendor and
  // refuses the save in its own words.
  const syncAdd = () => {
    submit.disabled = name.value.trim() === "";
  };
  name.addEventListener("input", syncAdd);
  syncAdd();
  submit.addEventListener("click", () => {
    // `id: null` is what makes this a create. An update carries the row's id
    // (see `accountRow`), and the two paths are one command so an update can
    // never land as a second row on the same credential.
    const args = { id: null, vendor: vendor.value, label: name.value, token: token.value };
    token.value = "";
    name.value = "";
    mutate("settings_save_account", args);
  });
  add.append(actionRow(submit), help(t.add.help));
  return [list, add];
}

function githubTab(t) {
  const org = group(t.org.heading);
  const input = textInput(t.org.value);
  org.append(field("github-org", t.org.label, input), help(t.org.help));
  const apply = button(t.org.saveLabel, "apply");
  // Its own command, not `settings_save_providers`: that one writes every
  // non-secret provider preference at once, so sending it from here would
  // blank every field this tab does not show.
  apply.addEventListener("click", () =>
    mutate("settings_save_github", { org: input.value })
  );
  org.appendChild(actionRow(apply));
  return [secretGroup(t.heading, t.secret), org];
}

function azureTab(t) {
  const box = group(t.budget.heading);
  const budget = numberInput(t.budget.value, 0);
  box.append(field("azure-budget", t.budget.label, budget), help(t.budget.help));
  const apply = button(t.budget.saveLabel, "apply");
  // `settings_save_providers` writes every non-secret provider preference in
  // one go, so the ones this tab doesn't show are sent back as they came --
  // a partial write would silently blank the Usage tab's fields.
  apply.addEventListener("click", () =>
    mutate("settings_save_providers", {
      prefs: {
        neonOrgId: S.view.usage.neon.orgId,
        sentryOrgSlug: S.view.usage.sentry.orgSlug,
        sentryMonthlyEventQuota: int(S.view.usage.sentry.quota),
        azureMonthlyBudgetUsd: Number(budget.value) || 0,
        neonUsdPerCuHour: Number(S.view.usage.neon.usdPerCuHour) || 0,
        neonUsdPerGibMonth: Number(S.view.usage.neon.usdPerGibMonth) || 0,
        vercelTeamId: S.view.usage.vercel.teamId,
      },
    })
  );
  box.appendChild(actionRow(apply));

  // Where the export lives. No credential group: the panel signs its own
  // read using the operator's Azure CLI session and stores nothing.
  const exp = group(t.export.heading);
  const account = textInput(t.export.account);
  const container = textInput(t.export.container);
  exp.append(
    field("azure-account", t.export.accountLabel, account),
    field("azure-container", t.export.containerLabel, container),
    help(t.export.help)
  );
  const saveExport = button(t.export.saveLabel, "apply");
  saveExport.addEventListener("click", () =>
    mutate("settings_save_azure", { account: account.value, container: container.value })
  );
  exp.appendChild(actionRow(saveExport));

  return [box, exp];
}

function usageTab(t) {
  // One group per provider, each carrying its own non-secret fields *and* its
  // credential -- the original tab's shape, where adding a provider is adding a
  // section.
  const neon = group(t.neon.heading);
  const orgId = textInput(t.neon.orgId);
  neon.appendChild(field("neon-org-id", t.neon.orgIdLabel, orgId));
  const cuRate = numberInput(t.neon.usdPerCuHour, 0);
  cuRate.step = "any"; // rates are fractional; the default step=1 would flag 0.106 invalid
  const gibRate = numberInput(t.neon.usdPerGibMonth, 0);
  gibRate.step = "any";
  neon.append(
    field("neon-usd-cu-hour", t.neon.usdPerCuHourLabel, cuRate),
    field("neon-usd-gib-month", t.neon.usdPerGibMonthLabel, gibRate),
    help(t.neon.ratesHelp)
  );
  secretControls(neon, t.neon.secret);

  const sentry = group(t.sentry.heading);
  const orgSlug = textInput(t.sentry.orgSlug);
  const quota = numberInput(t.sentry.quota, 0);
  sentry.append(
    field("sentry-org-slug", t.sentry.orgSlugLabel, orgSlug),
    field("sentry-quota", t.sentry.quotaLabel, quota),
    help(t.sentry.quotaHelp)
  );
  secretControls(sentry, t.sentry.secret);

  const vercel = group(t.vercel.heading);
  const teamId = textInput(t.vercel.teamId);
  vercel.append(
    field("vercel-team-id", t.vercel.teamIdLabel, teamId),
    help(t.vercel.teamIdHelp)
  );
  secretControls(vercel, t.vercel.secret);

  const apply = button(t.saveLabel, "apply");
  apply.addEventListener("click", () =>
    mutate("settings_save_providers", {
      prefs: {
        neonOrgId: orgId.value,
        sentryOrgSlug: orgSlug.value,
        sentryMonthlyEventQuota: int(quota.value),
        azureMonthlyBudgetUsd: Number(S.view.azure.budget.value) || 0,
        neonUsdPerCuHour: Number(cuRate.value) || 0,
        neonUsdPerGibMonth: Number(gibRate.value) || 0,
        vercelTeamId: teamId.value,
      },
    })
  );

  // One Apply for both, because `settings_save_providers` writes every
  // non-secret provider preference in one go.
  return [neon, sentry, vercel, actionRow(apply)];
}

/** One watched status page: its name, its address, the component it is watched
 *  through, and the two controls that pause or remove it. */
function vendorRow(t, vendor) {
  const row = node("div", "vendor-row");
  row.dataset.vendor = vendor.id;

  const head = node("div", "row");
  const names = node("div", "stack");
  names.append(node("span", "host-name", vendor.label), node("span", "dim", vendor.baseUrl));
  const component = node("div", "row");
  // The component's name as it was when it was picked, beside its own label --
  // assembled as two nodes rather than one string, because a template literal
  // here is this file writing a sentence Rust did not.
  component.append(node("span", "result", t.componentLabel), node("span", "result", vendor.component));
  names.appendChild(component);
  head.append(names, node("span", "grow"));

  const enabled = checkbox(vendor.enabled);
  enabled.addEventListener("change", () =>
    mutate("settings_set_status_vendor_enabled", { id: vendor.id, enabled: enabled.checked })
  );
  const remove = button(t.deleteLabel, "delete");
  remove.addEventListener("click", () =>
    mutate("settings_remove_status_vendor", { id: vendor.id })
  );
  head.append(enabled, remove);

  row.appendChild(head);
  return row;
}

/**
 * The Services tab: the status pages this cockpit watches, and the two-step
 * form that adds another.
 *
 * Two steps because a vendor is not a URL — it is a URL plus the one component
 * this stack depends on, and those ids are opaque (`k8w3r06qmzrp`) and
 * published nowhere a person would look. Step one asks the host; step two is a
 * picker over what it answered.
 *
 * Step two is built from `S.probe` and from nothing else, so it is reachable
 * only after a probe came back with components. A failed probe renders Rust's
 * sentence for what it found and leaves no picker at all: an empty picker would
 * be this file turning "we could not look" into "this page has no components",
 * which are different facts with different fixes.
 */
function servicesTab(t) {
  const list = group(t.heading);
  if (t.rows.length === 0) list.appendChild(help(t.empty));
  for (const vendor of t.rows) list.appendChild(vendorRow(t, vendor));

  const add = group(t.add.heading);
  // Refilled from the answer rather than from a copy this file keeps:
  // normalised when the probe validated it, exactly as typed when it did not,
  // so a rejected address is still on screen to be corrected.
  const url = textInput(S.probe ? S.probe.baseUrl : "");
  url.autocapitalize = "off";
  url.spellcheck = false;
  add.appendChild(field("vendor-url", t.add.urlLabel, url));

  const probe = button(t.add.probeLabel, "probe");
  const probing = node("p", "help", "");
  // Everything the probe produced, in one container so it can be dropped
  // without a re-render (see the `input` listener below).
  const found = node("div", "stack");

  const runProbe = async () => {
    probe.disabled = true;
    probing.textContent = t.add.probingLabel;
    const answer = await callRust("settings_probe_status_vendor", { baseUrl: url.value });
    // Null is the offline path (no Tauri and no fixture for this command).
    // Leave the form as it was rather than clearing what was typed.
    if (!answer) {
      probe.disabled = false;
      probing.textContent = "";
      return;
    }
    S.probe = answer;
    render();
  };
  probe.addEventListener("click", runProbe);
  url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") runProbe();
  });
  url.addEventListener("input", () => {
    if (!S.probe || url.value.trim() === S.probe.baseUrl) return;
    // A component list belongs to the address it was read from. Editing the
    // address after an answer would otherwise let the old host's component be
    // stored against the new host's URL — a row polling something nobody
    // chose. Dropped in place rather than by re-rendering, which would rebuild
    // the field under the caret and eat the rest of the word.
    S.probe = null;
    found.replaceChildren();
  });
  add.append(actionRow(probe), probing, help(t.add.help), found);

  // Rust's own sentence for what it found, verbatim and never merged with
  // another: "couldn't reach that host" and "that page is JSON but lists no
  // components" send an operator to two different fixes.
  if (S.probe && S.probe.reason) found.appendChild(node("p", "reason", S.probe.reason));

  if (S.probe && S.probe.components) {
    const options = S.probe.components.map((c) => ({ value: c.id, label: c.name }));
    const picker = select(options, options[0].value);
    const name = textInput("");
    found.append(
      field("vendor-component", t.add.componentSelectLabel, picker),
      help(t.add.componentHelp),
      field("vendor-name", t.add.nameLabel, name)
    );

    const save = button(t.add.saveLabel, "add");
    // A hint, not the validation: Rust re-checks all four fields and refuses
    // the save with its own reason.
    const syncSave = () => {
      save.disabled = name.value.trim() === "";
    };
    name.addEventListener("input", syncSave);
    syncSave();
    save.addEventListener("click", () => {
      const chosen = S.probe.components.find((c) => c.id === picker.value);
      const args = {
        // The address the components were actually read from, not the field --
        // the two are kept equal by the `input` listener above, and this is the
        // one that is true by construction.
        baseUrl: S.probe.baseUrl,
        label: name.value,
        // Both halves of the component or neither. A missing pair is sent as
        // empty strings and refused by Rust, rather than invented here.
        componentId: chosen ? chosen.id : "",
        componentLabel: chosen ? chosen.name : "",
      };
      // Reset at submit, like the Add Host form: a submitted field must not
      // ride the re-render as an unapplied edit.
      name.value = "";
      S.probe = null;
      mutate("settings_save_status_vendor", args);
    });
    found.appendChild(actionRow(save));
  }

  return [list, add];
}

/**
 * The OpenClaw tab: the gateway URL, the optional bearer token, and the device
 * pairing block.
 *
 * The pairing block is the only part of Settings built from *live* session
 * state, which is why every mutation here re-renders from the `{status,
 * settings}` answer like the rest of the surface — saving a URL restarts the
 * session, and the status row has to show what that produced.
 */
function openclawTab(t) {
  const gateway = group(t.heading);
  const url = textInput(t.gateway.value);
  url.placeholder = t.gateway.placeholder;
  url.autocapitalize = "off";
  url.spellcheck = false;
  gateway.appendChild(field("openclaw-gateway", t.gateway.label, url));

  const save = button(t.gateway.saveLabel, "apply");
  const submitUrl = () => mutate("settings_save_openclaw", { gatewayUrl: url.value });
  save.addEventListener("click", submitUrl);
  url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submitUrl();
  });
  gateway.appendChild(actionRow(save));
  // The bearer token rides the shared credential controls, so it is stored,
  // cleared and badged exactly like every other secret on this surface.
  secretControls(gateway, t.secret);

  const pairing = group(t.pairingHeading);

  const status = node("div", "row");
  status.dataset.row = "status";
  const statusValue = node("span", "result", t.status.text);
  statusValue.style.color = t.status.color;
  status.append(node("span", "lbl", t.statusLabel), node("span", "grow"), statusValue);
  pairing.appendChild(status);

  if (t.deviceId) {
    const device = node("div", "row");
    device.dataset.row = "device";
    // Selectable: the operator reads this fingerprint off the screen and
    // matches it against the gateway's device list.
    const value = node("span", "link-url", t.deviceId);
    device.append(node("span", "lbl", t.deviceLabel), node("span", "grow"), value);
    pairing.appendChild(device);
  } else {
    // Not an empty Device ID row: no key has been minted, and a blank value
    // would claim an identity that does not exist.
    pairing.appendChild(help(t.noDeviceLabel));
  }

  if (t.pairing) {
    const block = node("div", "stack");
    block.dataset.row = "pairing";
    block.appendChild(help(t.pairing.explanation));
    if (t.pairing.command) {
      const command = node("code", "oc-command", t.pairing.command);
      block.appendChild(command);
    }
    // The gateway's own remediation text, shown verbatim when it sent any.
    if (t.pairing.hint) block.appendChild(help(t.pairing.hint));
    const retry = button(t.pairing.retryLabel, "retry");
    retry.addEventListener("click", () => mutate("settings_openclaw_retry", {}));
    block.appendChild(actionRow(retry));
    pairing.appendChild(block);
  }

  return [gateway, pairing];
}

function aboutTab(t) {
  const box = group(null);
  box.append(
    node("h2", "about-name", t.name),
    node("p", "dim", t.version),
    node("p", "about-tagline", t.tagline)
  );
  // The URLs are shown as text, not as anchors. Following one would either
  // navigate the cockpit's own webview away from the app, or need the opener
  // plugin granted to the ACL -- and a link that silently does neither is
  // worse than a URL you can select and paste.
  for (const link of t.links) {
    const row = node("div", "link-row");
    row.append(node("span", "link-label", link.label), node("span", "link-url", link.url));
    box.appendChild(row);
  }
  box.appendChild(node("p", "dim", t.copyright));
  return [box];
}

function renderBody() {
  const body = $s("settingsBody");
  const build = {
    general: generalTab,
    layout: layoutTab,
    github: githubTab,
    accounts: accountsTab,
    portfolio: portfolioTab,
    hosts: hostsTab,
    azure: azureTab,
    usage: usageTab,
    services: servicesTab,
    openclaw: openclawTab,
    about: aboutTab,
  }[S.tab];
  body.replaceChildren(...(build ? build(S.view[S.tab]) : []));
}

function render() {
  $s("settingsTitle").textContent = S.view.title;
  $s("settingsClose").textContent = S.view.closeLabel;
  $s("settingsStatus").textContent = S.status;
  renderTabs();
  renderBody();
}

// MARK: open / close

async function openSettings() {
  // Offline (no Tauri), the same dumped-fixture path the cockpit uses, so the
  // surface can be opened in a plain browser and by the Playwright suite.
  const view = await callRust("settings_view", {}, "sample-settings.json");
  if (!view) return;
  S.view = view;
  S.status = "";
  // A probe answer must not outlive the session that ran it: reopening
  // Settings would otherwise show a component picker for an address nobody
  // just typed.
  S.probe = null;
  settingsOpen = true;
  $s("cockpitView").hidden = true;
  $s("settings").hidden = false;
  render();
}

async function closeSettings() {
  settingsOpen = false;
  $s("settings").hidden = true;
  $s("cockpitView").hidden = false;
  // Repaint at the real width now rather than up to a poll interval late: the
  // cockpit measured zero while it was hidden.
  await refreshCockpit();
  // ...and the panels, which keep their own timers. Without this a credential
  // or address saved a second ago leaves its panel still displaying the setup
  // instruction that asked for it.
  await refreshPanels();
}

$s("settingsToggle").addEventListener("click", openSettings);
$s("settingsClose").addEventListener("click", closeSettings);

// Test-only introspection, matching app.js's `window.__SOLADOR_TEST__`:
// read-only, and no production behaviour depends on it.
window.__SOLADOR_SETTINGS_TEST__ = { open: openSettings, close: closeSettings, tab: () => S.tab };

})();
