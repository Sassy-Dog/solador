// The Settings surface. Same discipline as the cockpit: every string here
// arrives from Rust (`app/src-tauri/src/settings.rs`) and this file does
// layout, wiring, and nothing else. A label typed into this file is a label
// that can drift from the Swift app without a test noticing.
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
  input.value = value === undefined || value === null ? "" : String(value);
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

/** Applies a mutation's `{status, settings}` answer. The frontend never
 *  patches its own copy -- it re-renders from what was actually persisted, so
 *  it cannot show an edit that failed to save. */
async function apply(result) {
  if (!result) return;
  S.view = result.settings;
  S.status = result.status || "";
  render();
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

  const overflow = select(g.hostOverflow.options, g.hostOverflow.value);
  box.append(field("general-overflow", g.hostOverflow.label, overflow), help(g.hostOverflow.help));

  const apply = button(g.saveLabel, "apply");
  apply.addEventListener("click", () =>
    mutate("settings_save_general", {
      refreshIntervalSecs: int(interval.value),
      coreRowSpan: int(span.value),
      hostOverflowMode: overflow.value,
    })
  );
  box.appendChild(actionRow(apply));
  return [box];
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
    // DOM.
    token.value = "";
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
 * is the port of Swift's re-read-on-access bindings: this file never assembles
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
    head.append(node("span", "slug", repo.slug), node("span", "grow"));

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
    list.appendChild(row);
  }

  const add = group(t.add.heading);
  const slug = textInput("");
  add.appendChild(field("repo-slug", t.add.slugLabel, slug));
  const submit = button(t.add.buttonLabel, "add");
  // Swift disables Add until the slug at least looks like `owner/name`; Rust
  // re-checks it (and rejects duplicates) either way -- this is a hint, not
  // the validation.
  const syncAdd = () => {
    submit.disabled = !slug.value.includes("/");
  };
  slug.addEventListener("input", syncAdd);
  syncAdd();
  const submitSlug = () => mutate("settings_add_repo", { slug: slug.value });
  submit.addEventListener("click", submitSlug);
  slug.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !submit.disabled) submitSlug();
  });
  add.append(actionRow(submit), help(t.add.help));
  return [list, add];
}

function azureTab(t) {
  const box = group(t.budget.heading);
  const budget = numberInput(t.budget.value, 0);
  box.append(field("azure-budget", t.budget.label, budget), help(t.budget.help));
  const apply = button(t.budget.saveLabel, "apply");
  // `settings_save_providers` writes all four non-secret provider preferences
  // at once, so the ones this tab doesn't show are sent back as they came --
  // a partial write would silently blank the Usage tab's fields.
  apply.addEventListener("click", () =>
    mutate("settings_save_providers", {
      neonOrgId: S.view.usage.neon.orgId,
      sentryOrgSlug: S.view.usage.sentry.orgSlug,
      sentryMonthlyEventQuota: int(S.view.usage.sentry.quota),
      azureMonthlyBudgetUsd: Number(budget.value) || 0,
    })
  );
  box.appendChild(actionRow(apply));
  return [box, secretGroup(t.secretHeading, t.secret)];
}

function usageTab(t) {
  // One group per provider, each carrying its own non-secret fields *and* its
  // credential -- the Swift tab's shape, where adding a provider is adding a
  // section.
  const neon = group(t.neon.heading);
  const orgId = textInput(t.neon.orgId);
  neon.appendChild(field("neon-org-id", t.neon.orgIdLabel, orgId));
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

  const apply = button(t.saveLabel, "apply");
  apply.addEventListener("click", () =>
    mutate("settings_save_providers", {
      neonOrgId: orgId.value,
      sentryOrgSlug: orgSlug.value,
      sentryMonthlyEventQuota: int(quota.value),
      azureMonthlyBudgetUsd: Number(S.view.azure.budget.value) || 0,
    })
  );

  // One Apply for both, because `settings_save_providers` writes every
  // non-secret provider preference in one go.
  return [neon, sentry, actionRow(apply)];
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
    github: (t) => [secretGroup(t.heading, t.secret)],
    portfolio: portfolioTab,
    hosts: hostsTab,
    azure: azureTab,
    usage: usageTab,
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
}

$s("settingsToggle").addEventListener("click", openSettings);
$s("settingsClose").addEventListener("click", closeSettings);

// Test-only introspection, matching app.js's `window.__DEVCANOPY_TEST__`:
// read-only, and no production behaviour depends on it.
window.__DEVCANOPY_SETTINGS_TEST__ = { open: openSettings, close: closeSettings, tab: () => S.tab };

})();
