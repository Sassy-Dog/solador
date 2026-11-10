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
// Tauri's ACL permits without a grant. Settings does not use the repository
// Actions URL permission granted to the cockpit's opener plugin.
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
  tab: "connections",
  editor: null,
  catalog: false,
  route: null,
  /** A source's steps in progress: `{kind, picked, added, index, secret,
   *  addTile, problem}`. Everything the operator chose so far lives here and
   *  nowhere else until Finish, so Cancel has nothing to undo. Dropped with the
   *  editor; the key, if one was typed, goes with it. */
  walk: null,
  status: "",
  /** Host id -> its last Test result line. Survives a re-render. */
  tests: new Map(),
  /** The last `settings_probe_status_vendor` answer, or `null` before anything
   *  has been probed. Held here like `tests` is: which host you last probed is
   *  a *view* state, not something the store remembers.
   *
   *  Rust's shape, unchanged — `{baseUrl, components, reason}`, where
   *  `components` is a non-empty list or `null` and `reason` is the finding or
   *  `null`. This file never fills either in. */
  probe: null,
  /** Host pairing (#448), by scope: `"add"` for the Add Host form, a host id
   *  for that host's edit form. Each is `{address, port, answer, trusted}` --
   *  `answer` is Rust's `settings_probe_host_certificate` payload verbatim, and
   *  `address`/`port` are what was typed when it was fetched, so an answer is
   *  only ever shown beside the endpoint it was read from. `trusted` is the
   *  Trust click and nothing else. Held here like `probe` is: a *view* state,
   *  never persisted by this file -- the store hears about a fingerprint only
   *  in the arguments of Add / Save, and only a trusted one. */
  pair: {},
  /** Host ids whose last Test said the certificate changed, so **Re-pair**
   *  appears the moment that is known rather than at the next Settings
   *  payload. Rust's `certificateChanged`, remembered per host. */
  repair: new Set(),
  /** Host ids whose last Test said the cockpit refused to send the token over
   *  plain HTTP (#449): an unpaired host off loopback and the tailnet. Lets the
   *  edit form explain itself and point at pairing the moment Test knows,
   *  as `repair` does. Rust's `plainRefused`, remembered per host. */
  unpaired: new Set(),
  /** The repos screen's search, keyed by account id: `{summary, query,
   *  answer, seq, active}`. `summary` is `"pending"` while the discovery walk
   *  runs, then Rust's `settings_discover_repos` payload verbatim (`reason` set
   *  when it failed); `answer` is the newest `settings_search_repos` payload
   *  painted; `seq` numbers the requests so a reply that lands after a newer
   *  one is dropped; `active` is the arrow-key row. Held here like `probe` is —
   *  a *view* state, never persisted — and it is what puts the box and its
   *  results back after a mutation re-renders the card. */
  search: {},
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
  // Every Settings text field is an identifier or a short name (hostnames,
  // slugs, IPs, tokens, URLs) — none is prose — so WebKit's system text-input
  // behaviour (autocapitalize, autocorrect, spellcheck) is opted out for all
  // of them here, once, rather than per field (#435). `autocorrect` is a
  // WebKit extension with no IDL property, so it is set as an attribute.
  input.autocapitalize = "off";
  input.setAttribute("autocorrect", "off");
  input.spellcheck = false;
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

function select(options, value, { saveOnChange = false } = {}) {
  const sel = node("select", "input");
  for (const option of options) {
    const opt = node("option", null, option.label);
    opt.value = String(option.value);
    sel.appendChild(opt);
  }
  sel.value = String(value);
  sel.dataset.initialValue = sel.value;
  if (saveOnChange) sel.dataset.saveOnChange = "true";
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
  for (const el of document.querySelectorAll("#settings input[id], #settings select[id]")) {
    if (el.type === "password") continue;
    if (el.type === "checkbox") {
      if (el.dataset.applyGated === "true" && el.checked !== el.defaultChecked) edits.push([el.id, el.checked]);
      continue;
    }
    // Immediate saves must render Rust's answer, including a refused change.
    if (el.dataset.saveOnChange === "true") continue;
    const initial = el.tagName === "SELECT" ? el.dataset.initialValue : el.defaultValue;
    if (el.value !== initial) edits.push([el.id, el.value]);
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
    if (el.type === "checkbox") el.checked = value;
    else el.value = value;
    el.dispatchEvent(new Event("input"));
  }
}

function renderKeepingEdits() {
  const edits = unappliedEdits();
  render();
  restoreEdits(edits);
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
  const expanded = Array.from(document.querySelectorAll("#settingsBody .connection-advanced"), el => el.open);
  // A field that asks for it gets its focus back across the render, caret at
  // the end: Enter in the repo search ticks a row, and the operator's next
  // keystroke belongs in the same box.
  const focused = document.activeElement;
  const refocus = focused?.dataset?.keepFocus === "true" ? focused.id : null;
  S.view = result.settings;
  S.status = result.status || "";
  render();
  restoreEdits(edits);
  document.querySelectorAll("#settingsBody .connection-advanced").forEach((el, index) => { el.open = expanded[index] || false; });
  const again = refocus && document.getElementById(refocus);
  if (again && !again.disabled) {
    again.focus();
    again.setSelectionRange(again.value.length, again.value.length);
  }
}

async function mutate(command, args, submitted = []) {
  const submittedValues = submitted.map(input => [input,
    input.type === "checkbox" ? input.checked : input.value]);
  const previous = new Set(S.view.connections.rows.map(row => row.id));
  const editor = S.editor;
  let result;
  try { result = await callRust(command, args); }
  catch {
    S.status = S.view.connections.saveFailed;
    $s("settingsStatus").textContent = S.status;
    return;
  }
  if (result?.settings && editor && S.editor === editor && !editor.entityId) {
    const created = result.settings.connections.rows.find(row =>
      row.kind === editor.kind && !previous.has(row.id));
    if (created) S.editor = { ...created };
  }
  if (result?.saved) {
    for (const [input, value] of submittedValues) {
      if (input.type === "checkbox") input.defaultChecked = value;
      else input.defaultValue = value;
    }
  }
  apply(result);
}

// MARK: rendering

function renderTabs() {
  const bar = $s("settingsTabs");
  bar.setAttribute("aria-label", S.view.title);
  bar.replaceChildren();
  for (const tab of S.view.tabs) {
    const b = button(tab.title, "tab");
    b.dataset.tab = tab.id;
    if (tab.id === S.tab) b.dataset.active = "true";
    b.setAttribute("aria-current", tab.id === S.tab ? "page" : "false");
    if (tab.id === "connections" && S.view.connections.attentionCount) {
      b.appendChild(node("span", "connection-count", S.view.connections.attentionCount));
    }
    b.addEventListener("click", () => navigate(() => {
      S.tab = tab.id;
      S.editor = null;
      S.walk = null;
      S.catalog = false;
      S.route = null;
      S.status = "";
      S.search = {};
      render();
    }));
    bar.appendChild(b);
  }
}

/** One credential: a password field, Save, Clear, and a badge that says
 *  whether something is stored -- never what. The value is cleared from the
 *  field the moment it is handed to Rust, and no payload ever brings one
 *  back, so a stored credential has no path into the DOM. */
function secretControls(box, secret) {
  box.dataset.secret = secret.key;
  const labels = S.view.connections;
  const summary = node("div", "credential-summary row");
  summary.append(node("span", "lbl", secret.fieldLabel), node("span", "grow"));
  if (secret.stored) summary.appendChild(node("span", "badge-ok", secret.storedLabel));
  const replace = button(secret.stored ? labels.replaceLabel : labels.addCredentialLabel, "replace-secret");
  const edit = node("div", "credential-editor stack");
  edit.hidden = true;
  const input = textInput("", "password");
  input.autocomplete = "new-password";
  edit.appendChild(field(`secret-${secret.key}`, secret.fieldLabel, input));
  replace.addEventListener("click", () => {
    edit.hidden = false;
    replace.hidden = true;
    input.focus();
  });

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
  const cancel = button(labels.cancelLabel, "credential-cancel");
  cancel.addEventListener("click", () => {
    input.value = "";
    edit.hidden = true;
    replace.hidden = false;
    replace.focus();
  });
  row.append(cancel);
  summary.append(replace);
  edit.append(row, help(secret.help));
  box.append(summary, edit);
  return box;
}

/** A group that holds nothing but one credential. */
function secretGroup(heading, secret) {
  return secretControls(group(heading), secret);
}

function generalTab(g) {
  const box = group(g.heading);

  // Where the shared Refresh Interval went (#615): Rust says, this paints.
  box.append(help(g.cadenceNote));

  return [box, crashReportingGroup(g.crashReporting)];
}

/** The crash-reporting opt-in.
 *
 *  Its own group, and it saves on `change` rather than waiting for the Apply
 *  button above — like the Portfolio and Hosts checkboxes, and for a stronger
 *  reason than either: consent is not a draft. Someone who unticks this box and
 *  closes the window has withdrawn permission, and an unpressed Apply is not a
 *  defensible reason to keep reporting. (Checkboxes are excluded from
 *  `unappliedEdits` for exactly this reason — there is no such thing as an
 *  unapplied one.)
 *
 *  The status line is Rust's sentence, not a string built here: "on" and "is
 *  actually reporting" are different facts, and only Rust knows the second. */
function crashReportingGroup(c) {
  const box = group(c.heading);
  const toggle = checkbox(c.value);
  toggle.addEventListener("change", () =>
    mutate("settings_set_crash_reporting", { enabled: toggle.checked })
  );
  box.appendChild(field("general-crash-reporting", c.label, toggle));
  box.appendChild(node("p", "help crash-status", c.status));
  box.appendChild(help(c.help));
  return box;
}

/**
 * The pairing step (#448): fetch the certificate an address presents, show its
 * fingerprint, and let the operator **Trust** it.
 *
 * This file decides nothing. Rust fetched the certificate without trusting it
 * and worded what it found; the operator compares the fingerprint with what
 * `solador-agent tls-fingerprint` printed on the host; and only the Trust click
 * marks it. Nothing here is persisted -- `trusted()` is what the caller puts in
 * the arguments of Add / Save, and an untrusted answer is never sent.
 *
 * An answer belongs to the address and port it was read from: editing either
 * drops it, so a fingerprint fetched from one machine can never be trusted
 * against another.
 *
 * `onChange` is not called while the block first paints, so a caller may
 * construct this before the controls it updates exist.
 */
function pairing(t, scope, addressInput, portInput, { checkLabel, onChange = () => {} } = {}) {
  const labels = t.pair;
  const box = node("div", "pairing stack");
  const check = button(checkLabel || labels.checkLabel, "pair-check");
  const message = node("p", "help pair-message", "");
  const found = node("div", "stack pair-found");

  /** The stored answer, if it was read from what is typed now. */
  const current = () => {
    const entry = S.pair[scope];
    const same = entry && entry.address === addressInput.value.trim() &&
      entry.port === portInput.value.trim();
    return same ? entry : null;
  };

  const paint = (notify) => {
    found.replaceChildren();
    const entry = current();
    message.textContent = entry ? entry.answer.message : "";
    if (entry && entry.answer.status === "tls") {
      const fingerprint = node("code", "pair-fingerprint", entry.answer.fingerprint);
      fingerprint.tabIndex = 0;
      found.append(node("span", "lbl", labels.fingerprintLabel), fingerprint);
      if (entry.trusted) {
        found.appendChild(node("p", "help pair-trusted", labels.trustedLabel));
      } else {
        const trust = button(labels.trustLabel, "pair-trust primary");
        trust.addEventListener("click", () => {
          entry.trusted = true;
          paint(true);
        });
        found.appendChild(actionRow(trust));
      }
    }
    if (notify) onChange();
  };

  check.addEventListener("click", async () => {
    check.disabled = true;
    message.textContent = labels.checkingLabel;
    const address = addressInput.value.trim();
    const port = portInput.value.trim();
    const answer = await callRust("settings_probe_host_certificate", { address, port });
    check.disabled = false;
    // Null is the offline path (no Tauri and no fixture for this command).
    if (!answer) {
      message.textContent = "";
      return;
    }
    S.pair[scope] = { address, port, answer, trusted: false };
    paint(true);
  });
  for (const input of [addressInput, portInput]) {
    input.addEventListener("input", () => {
      // Dropped in place rather than by re-rendering, which would rebuild the
      // field under the caret and eat the rest of the word.
      if (S.pair[scope] && !current()) {
        delete S.pair[scope];
        paint(true);
      }
    });
  }
  paint(false);
  box.append(actionRow(check), help(labels.help), message, found);
  return {
    node: box,
    /** The fingerprint the operator trusted for what is typed now, or null. */
    trusted() {
      const entry = current();
      return entry && entry.trusted && entry.answer.status === "tls"
        ? entry.answer.fingerprint
        : null;
    },
    /** A certificate was found for what is typed and has not been trusted. */
    pending() {
      const entry = current();
      return !!entry && entry.answer.status === "tls" && !entry.trusted;
    },
  };
}

function hostsTab(t, options = {}) {
  const list = group(options.edit ? null : t.heading);
  if (t.rows.length === 0) {
    list.appendChild(help(t.empty));
  }
  for (const host of t.rows) {
    const row = node("div", "host-row");
    row.dataset.host = host.id;

    if (options.edit) row.appendChild(hostDetails(t, host));

    const head = node("div", "row");
    const names = node("div", "stack");
    names.append(node("span", "host-name", host.name), node("span", "dim", host.endpoint));
    // Where this host's agent stands against the newest verified release (#489),
    // persistent, without pressing Test. The state's name, the sentence and the
    // colour are all Rust's (`viewmodel::agent_release`); the colour goes on
    // through the CSSOM because a `style=""` attribute is blocked by the CSP.
    const release = node("span", "agent-release", host.agentRelease.text);
    release.dataset.state = host.agentRelease.state;
    release.style.color = host.agentRelease.color;
    names.appendChild(release);
    // The pin, in full, where the operator can compare it with the host.
    if (host.pinned) {
      const pinned = node("span", "dim pinned-line", t.pair.pinnedLabel + ": ");
      pinned.appendChild(node("code", "pair-fingerprint pinned-fingerprint", host.fingerprint));
      names.appendChild(pinned);
    }
    // Its own line under the buttons, not a slot in `names`: the sentence gets
    // the row's whole width, and when it wraps nothing above it moves.
    const result = node("span", "result", S.tests.get(host.id) || "");
    head.append(names);
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
      // The certificate-changed finding is what offers Re-pair; repaint only
      // when it flips, so an ordinary Test never rebuilds the form.
      const flagged = !!answer.certificateChanged;
      const refused = !!answer.plainRefused;
      if (flagged !== S.repair.has(answer.id) || refused !== S.unpaired.has(answer.id)) {
        if (flagged) S.repair.add(answer.id);
        else S.repair.delete(answer.id);
        if (refused) S.unpaired.add(answer.id);
        else S.unpaired.delete(answer.id);
        renderKeepingEdits();
      }
    });

    const enabled = checkbox(host.enabled);
    enabled.setAttribute("aria-label", S.view.connections.monitorLabel);
    enabled.addEventListener("change", () =>
      mutate("settings_set_host_enabled", { id: host.id, enabled: enabled.checked })
    );

    const remove = button(t.deleteLabel, "delete");
    remove.addEventListener("click", () => {
      S.tests.delete(host.id);
      mutate("settings_remove_host", { id: host.id });
    });

    const monitor = node("label", "connection-toggle");
    monitor.append(enabled, node("span", null, S.view.connections.monitorLabel));
    head.append(test, remove);
    row.prepend(head, result);
    row.appendChild(monitor);
    list.appendChild(row);
  }

  const boxes = options.addOnly ? [] : [list];

  if (options.edit) {
    return boxes;
  }
  const add = group(t.add.heading);
  const name = textInput("");
  const address = textInput("");
  const port = textInput(t.add.portDefault);
  const token = textInput("", "password");
  // Built before `submit` exists on purpose: it does not call back while it
  // first paints, and `syncAdd` below runs once everything does.
  const pair = pairing(t, "add", address, port, { onChange: () => syncAdd() });
  add.append(
    field("host-name", t.add.nameLabel, name),
    field("host-address", t.add.addressLabel, address),
    field("host-port", t.add.portLabel, port),
    field("host-token", t.add.tokenLabel, token),
    pair.node
  );

  const submit = button(t.add.buttonLabel, "add");
  // A certificate that was found and not trusted holds Add back: adding the
  // host anyway would poll a TLS agent over plain HTTP and show it as
  // unreachable, which is the failure the pairing step exists to prevent.
  const syncAdd = () => {
    submit.disabled = name.value.trim() === "" || address.value.trim() === "" || pair.pending();
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
    // Only a fingerprint the operator trusted for THIS address and port is
    // ever sent; an unpaired host sends no key at all and stays plain HTTP.
    const trusted = pair.trusted();
    if (trusted) args.tlsFingerprint = trusted;
    // Dropped before the round-trip, not after: the token has no reason to
    // outlive the call, and a rejected save must not leave it sitting in the
    // DOM. The other three reset so the form comes back empty rather than
    // riding the re-render as unapplied edits.
    token.value = "";
    name.value = "";
    address.value = "";
    port.value = t.add.portDefault;
    delete S.pair.add;
    mutate("settings_add_host", args);
  });
  add.append(actionRow(submit), help(t.add.help));
  boxes.push(add);
  return boxes;
}

function hostDetails(t, host) {
  const labels = S.view.connections;
  const box = node("div", "host-details stack");
  const fields = node("div", "connection-fields");
  const name = textInput(host.name);
  const address = textInput(host.address);
  const port = textInput(host.port);
  const token = textInput("", "password");
  token.autocomplete = "new-password";
  fields.append(field("host-edit-name", labels.hostNameLabel, name),
    field("host-edit-address", labels.hostAddressLabel, address),
    field("host-edit-port", labels.hostPortLabel, port));
  const replace = button(host.tokenStored ? labels.replaceLabel : labels.addCredentialLabel, "host-replace-token");
  const credential = field("host-edit-token", t.add.tokenLabel, token);
  credential.hidden = true;
  // The field carries its own label, so the caption goes with the button
  // rather than reading "Agent token" twice.
  const caption = actionRow(node("span", "lbl", t.add.tokenLabel), replace);
  replace.addEventListener("click", () => {
    credential.hidden = false;
    caption.hidden = true;
    token.focus();
  });
  // Pairing (#448). An unpaired host can be paired; a paired host's pin is
  // replaced only after the cockpit has said its certificate changed -- the
  // operator does not re-pair a healthy host, and a healthy host cannot be
  // talked out of its pin by a stray click.
  const changed = host.certificateChanged || S.repair.has(host.id);
  let pair = null;
  if (!host.pinned || changed) {
    pair = pairing(t, host.id, address, port, {
      checkLabel: host.pinned ? t.pair.repairLabel : t.pair.checkLabel,
    });
  }
  const save = button(labels.saveLabel, "host-save primary");
  save.addEventListener("click", () => {
    const args = { id: host.id, name: name.value, address: address.value, port: port.value, token: token.value };
    // Present only when the operator trusted a fingerprint here: absent leaves
    // the pin exactly as it is, so a rename can never touch it.
    const trusted = pair && pair.trusted();
    if (trusted) args.tlsFingerprint = trusted;
    token.value = "";
    delete S.pair[host.id];
    S.repair.delete(host.id);
    S.unpaired.delete(host.id);
    mutate("settings_save_host", args);
  });
  box.append(fields);
  if (pair) {
    if (changed) box.appendChild(help(t.pair.repairHelp));
    // An unpaired host the cockpit will not put the token on plain HTTP for
    // (#449): say why it is not being polled, right above the step that fixes it.
    if (!host.pinned && (host.plainRefused || S.unpaired.has(host.id))) {
      box.appendChild(node("p", "help pair-unpaired", t.pair.unpairedHelp));
    }
    box.appendChild(pair.node);
  }
  box.append(caption, credential, actionRow(save));
  return box;
}

/** One repo's Configure… disclosure: watched workflows, the account picker,
 *  and Delete. Built up-front and toggled with `.hidden` — the delete-confirm
 *  precedent, applied to a form. */
function repoConfig(t, account, repo) {
  const box = node("div", "stack hidden-row");
  box.hidden = true;
  box.dataset.disclosure = "configure";
  const workflows = textInput(repo.workflows);
  workflows.disabled = !repo.enabled;
  // `change`, not `input`: a per-keystroke save re-renders under the caret.
  workflows.addEventListener("change", () =>
    mutate("settings_set_repo_workflows", { slug: repo.slug, workflows: workflows.value })
  );
  box.appendChild(field(`repo-workflows-${repo.slug}`, t.workflowsLabel, workflows));
  if (t.accountOptions.length > 0) {
    const picker = select(t.accountOptions, account ? account.id : "", { saveOnChange: true });
    picker.addEventListener("change", () =>
      mutate("settings_set_repo_account", { slug: repo.slug, accountId: picker.value || null })
    );
    box.appendChild(field(`repo-account-${repo.slug}`, t.accountLabel, picker));
  }
  const remove = button(t.deleteLabel, "repo-delete");
  remove.addEventListener("click", () => mutate("settings_remove_repo", { slug: repo.slug }));
  box.appendChild(actionRow(remove));
  return box;
}

/** One tracked repo: a compact line, with everything editable behind
 *  Configure…. `account` is null in the unattributed section. */
function repoRow(t, account, repo) {
  const row = node("div", "repo-row");
  row.dataset.repo = repo.slug;
  const head = node("div", "row");
  head.append(node("span", "slug", repo.slug));
  if (repo.accountMissing) head.appendChild(node("span", "badge-dim", t.missingAccountLabel));
  head.appendChild(node("span", "grow"));
  const enabled = checkbox(repo.enabled);
  enabled.addEventListener("change", () =>
    mutate("settings_set_repo_enabled", { slug: repo.slug, enabled: enabled.checked })
  );
  const config = repoConfig(t, account, repo);
  const configure = button(t.configureLabel, "configure");
  configure.addEventListener("click", () => {
    config.hidden = !config.hidden;
  });
  head.append(enabled, configure);
  row.append(head, config);
  return row;
}

/** A slug with the matched part in bold. `matchStart`/`matchEnd` are Rust's
 *  char offsets; `Array.from` walks the same code points, and the pieces go
 *  in as text nodes, never markup. */
function matchedSlug(entry) {
  const slug = node("span", "slug");
  const start = entry.matchStart;
  const end = entry.matchEnd;
  if (typeof start !== "number" || typeof end !== "number") {
    slug.textContent = entry.slug;
    return slug;
  }
  const chars = Array.from(entry.slug);
  slug.append(
    chars.slice(0, start).join(""),
    node("b", null, chars.slice(start, end).join("")),
    chars.slice(end).join("")
  );
  return slug;
}

/** One search result: a checkbox over "this account watches it".
 *
 *  The checkbox and the *watching* badge read the LIVE view (`account.repos`),
 *  never the answer — an answer is a snapshot, and painting its `tracked`
 *  after a toggle round-trip would show the state from before the click.
 *  Everything else on the row (grants, another account's claim, the untrack
 *  consequence) is the answer's. */
function searchRow(t, account, entry) {
  const row = node("div", "search-row");
  row.dataset.pick = entry.slug;
  const head = node("div", "row");
  const tracked = account.repos.some(
    (repo) => repo.slug.toLowerCase() === entry.slug.toLowerCase()
  );
  const box = checkbox(tracked);
  box.setAttribute("aria-label", entry.slug);
  head.append(box, matchedSlug(entry));
  if (tracked) head.appendChild(node("span", "badge-ok", t.watchingLabel));
  if (entry.archived) head.appendChild(node("span", "badge-dim", t.archivedLabel));
  if (!entry.granted) head.appendChild(node("span", "badge-dim", t.notGrantedLabel));
  if (entry.trackedBy) {
    // Two nodes, never a template literal: a composed sentence here would be
    // this file writing copy Rust did not.
    head.append(node("span", "dim", t.fetchedByLabel), node("span", "dim", entry.trackedBy));
    box.disabled = true;
  }
  head.appendChild(node("span", "grow"));

  // The untrack consequence is Rust's sentence, shown verbatim before the
  // remove goes through — the account-delete confirm, one row down.
  const confirm = node("div", "stack");
  confirm.dataset.confirm = "untrack";
  confirm.hidden = true;
  const setTracked = (want) =>
    mutate("settings_set_repo_tracked", { id: account.id, slug: entry.slug, tracked: want });
  box.addEventListener("change", () => {
    if (!box.checked && entry.untrackPrompt) {
      box.checked = true;
      box.disabled = true;
      confirm.hidden = false;
      return;
    }
    setTracked(box.checked);
  });
  const proceed = button(t.untrackLabel, "untrack-proceed");
  proceed.addEventListener("click", () => setTracked(false));
  const cancel = button(t.cancelLabel, "untrack-cancel");
  cancel.addEventListener("click", () => {
    confirm.hidden = true;
    box.disabled = false;
  });
  confirm.append(node("p", "confirm", entry.untrackPrompt || ""), actionRow(proceed, cancel));
  row.append(head, confirm);
  return row;
}

/** What sits under the search box for the newest answer: the rows, then how
 *  many matched, how many more there are, and Add by name. Nothing for an
 *  empty box: the watched list above already covers that case. */
function searchResults(t, account) {
  const state = S.search[account.id];
  const answer = state?.answer;
  if (!answer || !state.query.trim()) return [];
  const out = (answer.results || []).map((entry) => searchRow(t, account, entry));
  out.forEach((row, index) => {
    if (index === state.active) row.dataset.active = "true";
  });
  if (answer.reason) out.push(node("p", "confirm", answer.reason));
  if (answer.countLabel) out.push(node("span", "result search-count", answer.countLabel));
  if (answer.moreLabel) out.push(node("span", "dim search-more", answer.moreLabel));
  if (answer.addByName) {
    const add = button(answer.addByName.label, "repo-add");
    add.addEventListener("click", () => {
      // Submitted, so the box empties before the round trip, as the retired
      // slug field did: the repo it named is in the watched list next.
      state.query = "";
      state.answer = null;
      state.active = -1;
      mutate("settings_add_repo", { slug: answer.addByName.slug, accountId: account.id });
    });
    out.push(actionRow(add));
  }
  return out;
}

/** Repaints one account's results in place — never the whole card, which
 *  would take the caret out of the box on every keystroke. Reads the account
 *  from the live view, so a reply that lands after a mutation paints the
 *  checkboxes as they are now. */
function paintSearch(accountId) {
  const results = document.querySelector(`[data-search-results="${accountId}"]`);
  const t = S.view.accounts;
  const account = t.rows.find((row) => row.id === accountId);
  if (!results || !account) return;
  results.replaceChildren(...searchResults(t, account));
}

/** Moves the arrow-key highlight, starting at the first row. */
function moveActive(accountId, step) {
  const state = S.search[accountId];
  const rows = document.querySelectorAll(`[data-search-results="${accountId}"] .search-row`);
  if (!state || rows.length === 0) return;
  state.active = state.active < 0 ? 0 : Math.min(rows.length - 1, Math.max(0, state.active + step));
  rows.forEach((row, index) => {
    if (index === state.active) row.dataset.active = "true";
    else delete row.dataset.active;
  });
  rows[state.active].scrollIntoView({ block: "nearest" });
}

/** The walk behind the search, once per opening of the account: the list
 *  stays in Rust, and this file keeps only its summary. A reply for an
 *  account the operator has since left paints nothing. */
async function discoverRepos(accountId) {
  const state = S.search[accountId];
  let summary = null;
  try {
    summary = await callRust("settings_discover_repos", { id: accountId });
  } catch {
    summary = null;
  }
  if (S.search[accountId] !== state) return;
  // Every outcome Rust knows of comes back as a payload with its own
  // `reason`; only a refused call has none, and it says so rather than
  // leaving the pending line up.
  state.summary = summary && typeof summary === "object"
    ? summary
    : { reason: S.view.accounts.discoverFailedLabel };
  const t = S.view.accounts;
  const account = t.rows.find((row) => row.id === accountId);
  if (!account) return;
  // In place, like the results: a whole-card render here would pull the
  // caret out of whatever the operator started typing while the walk ran.
  document.querySelector(`[data-repo-search="${accountId}"]`)?.replaceWith(repoSearch(t, account));
  document.querySelector(`[data-discovered-orgs="${accountId}"]`)?.replaceWith(discoveredOrgs(t, account));
}

/** Add a repo: one box searched as the operator types, over everything the
 *  account's token can read (Rust's `settings_search_repos`). Disabled until
 *  the walk answers, with the pending line under it; then the walk's own
 *  count, or its reason when it failed — the box stays open then, because a
 *  full owner/name can still be added by name. */
function repoSearch(t, account) {
  const state = S.search[account.id];
  const box = node("div", "stack repo-search");
  box.dataset.repoSearch = account.id;
  const input = textInput(state.query);
  input.placeholder = t.searchPlaceholder;
  // A query is not an edit: `unappliedEdits` skips it (no unsaved prompt,
  // no restore), and `S.search` is what puts it back after a render.
  input.dataset.saveOnChange = "true";
  input.dataset.keepFocus = "true";
  const pending = state.summary === "pending";
  input.disabled = pending;
  box.appendChild(field(`repo-search-${account.id}`, t.searchLabel, input));
  if (pending) {
    box.appendChild(node("span", "result repo-search-note", t.discoverPendingLabel));
  } else if (state.summary.reason) {
    box.appendChild(node("p", "confirm", state.summary.reason));
  } else {
    box.appendChild(node("span", "result repo-search-note", state.summary.totalLabel));
    if (state.summary.truncatedNote) box.appendChild(help(state.summary.truncatedNote));
  }
  const results = node("div", "stack search-results");
  results.dataset.searchResults = account.id;
  results.append(...searchResults(t, account));
  box.appendChild(results);

  input.addEventListener("input", async () => {
    state.query = input.value;
    state.active = -1;
    const seq = ++state.seq;
    if (!input.value.trim()) {
      state.answer = null;
      paintSearch(account.id);
      return;
    }
    let answer;
    try {
      answer = await callRust("settings_search_repos", { id: account.id, query: input.value });
    } catch {
      return;
    }
    // Only the newest request paints, and only while the box still says what
    // it asked: a slow reply for "cl" never lands on top of "claims".
    if (S.search[account.id] !== state || seq !== state.seq || answer?.query !== state.query) return;
    state.answer = answer;
    paintSearch(account.id);
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      moveActive(account.id, event.key === "ArrowDown" ? 1 : -1);
    } else if (event.key === "Enter") {
      const active = document.querySelector(
        `[data-search-results="${account.id}"] .search-row[data-active="true"] .toggle`
      );
      if (!active) return;
      event.preventDefault();
      // The click a pointer would make, so the untrack two-step still applies.
      active.click();
    } else if (event.key === "Escape") {
      event.preventDefault();
      input.value = "";
      state.query = "";
      state.answer = null;
      state.active = -1;
      paintSearch(account.id);
    }
  });
  return box;
}

/** The organizations the walk derived that this account doesn't watch yet,
 *  offered beside the ones it does. Which are watched reads the live view
 *  (`account.orgs`), so a Watch click stops the offer on the next render;
 *  who else watches one is the walk's. */
function discoveredOrgs(t, account) {
  const box = node("div", "stack");
  box.dataset.discoveredOrgs = account.id;
  const summary = S.search[account.id]?.summary;
  const watched = (name) => account.orgs.some((org) => org.toLowerCase() === name.toLowerCase());
  const offered = (summary && summary !== "pending" && summary.orgs || []).filter((org) => !watched(org.name));
  box.hidden = offered.length === 0;
  for (const org of offered) {
    const orgRow = node("div", "row");
    orgRow.dataset.discoveredOrg = org.name;
    orgRow.append(node("span", "result", org.name));
    if (org.selectedBy) {
      orgRow.append(node("span", "dim", t.watchedByLabel), node("span", "dim", org.selectedBy));
    } else {
      orgRow.appendChild(node("span", "grow"));
      const watch = button(t.orgAddButtonLabel, "org-watch");
      watch.addEventListener("click", () =>
        mutate("settings_set_account_org", { id: account.id, org: org.name, selected: true })
      );
      orgRow.appendChild(watch);
    }
    box.appendChild(orgRow);
  }
  return box;
}

/** One vendor account as a card: identity and badge on top, the repos it
 *  fetches and the organizations it watches below. Its token, name and
 *  organizations are edited in the account's steps (Rust's `walk`), reached from
 *  the button here, and saved together at Finish; what stays on the card is what
 *  is per repo and repeatable. */
function accountCard(t, account) {
  const card = group(account.label);
  card.dataset.account = account.id;

  const head = node("div", "row");
  head.append(
    node("span", "dim", account.vendorLabel),
    node("span", "grow"),
    // Three answers: a store that would not answer is neither of the others.
    account.unread
      ? node("span", "badge-dim", t.notReadLabel)
      : node("span", account.stored ? "badge-ok" : "badge-dim",
        account.stored ? t.tokenStoredLabel : t.noTokenLabel)
  );
  const enabled = checkbox(account.enabled);
  enabled.setAttribute("aria-label", S.view.connections.monitorLabel);
  enabled.addEventListener("change", () =>
    mutate("settings_set_account_enabled", { id: account.id, enabled: enabled.checked })
  );

  const edit = button(t.editLabel, "edit-account");
  edit.addEventListener("click", () => {
    const row = S.view.connections.rows.find(item => item.entityId === account.id);
    if (row) openConnection({ ...row });
  });
  head.append(edit);
  const monitor = node("label", "connection-toggle");
  monitor.append(enabled, node("span", null, S.view.connections.monitorLabel));

  // The repos this account fetches, under one heading per owner the way the
  // Repos tile groups them (Rust's `repoGroups`), everything editable behind
  // the per-row Configure…; then the search that adds more.
  const repos = node("div", "stack");
  repos.dataset.repos = account.id;
  repos.appendChild(node("span", "lbl", t.reposHeading));
  if (account.repos.length === 0) {
    repos.appendChild(node("span", "result", t.noReposLabel));
  }
  for (const owner of account.repoGroups) {
    const heading = node("span", "repo-owner", owner.label);
    heading.dataset.owner = owner.owner;
    repos.appendChild(heading);
    for (const repo of owner.repos) repos.appendChild(repoRow(t, account, repo));
  }
  // Opening the account is what starts its walk — once: the state outlives
  // every re-render until the operator leaves the page.
  if (!S.search[account.id]) {
    S.search[account.id] = { summary: "pending", query: "", answer: null, seq: 0, active: -1 };
    discoverRepos(account.id);
  }
  repos.appendChild(repoSearch(t, account));
  repos.appendChild(help(t.reposHelp));

  // Runner organizations: the stored selection, read-only (it is edited in the
  // steps), and the organizations the discovery derived that this account
  // doesn't watch yet, offered with a Watch.
  const orgs = node("div", "stack");
  orgs.dataset.orgs = account.id;
  orgs.appendChild(node("span", "lbl", t.orgsHeading));
  if (account.orgs.length === 0) {
    orgs.appendChild(node("span", "result", t.noOrgsLabel));
  }
  for (const org of account.orgs) {
    const orgRow = node("div", "row");
    orgRow.dataset.org = org;
    orgRow.appendChild(node("span", "result", org));
    orgs.appendChild(orgRow);
  }
  orgs.appendChild(discoveredOrgs(t, account));

  card.append(head, monitor, repos, orgs);
  return card;
}

/**
 * The GitHub accounts' cards: one per account, and the unattributed repos no
 * card can claim. Creating, changing and removing an account are the steps'
 * (Add source, or the account's row), so nothing here writes a token, a name or
 * an organization list. Nothing here re-homes an orphaned repo onto a surviving
 * account — that would be this file inventing an owner, and Rust refuses to do
 * it for the same reason.
 */
function accountsTab(t) {
  const out = [];
  for (const account of t.rows) out.push(accountCard(t, account));

  if (t.unattributed.length > 0) {
    const orphans = group(t.unattributedHeading);
    for (const repo of t.unattributed) orphans.appendChild(repoRow(t, null, repo));
    orphans.appendChild(help(t.unattributedHelp));
    out.push(orphans);
  }
  return out;
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
  enabled.setAttribute("aria-label", S.view.connections.monitorLabel);
  enabled.addEventListener("change", () =>
    mutate("settings_set_status_vendor_enabled", { id: vendor.id, enabled: enabled.checked })
  );
  const remove = button(t.deleteLabel, "delete");
  remove.addEventListener("click", () =>
    mutate("settings_remove_status_vendor", { id: vendor.id })
  );
  const monitor = node("label", "connection-toggle");
  monitor.append(enabled, node("span", null, S.view.connections.monitorLabel));
  head.append(remove);

  row.append(head, monitor);
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
function servicesTab(t, options = {}) {
  const list = group(t.heading);
  if (t.rows.length === 0) list.appendChild(help(t.empty));
  for (const vendor of t.rows) list.appendChild(vendorRow(t, vendor));
  if (options.edit) return [list];

  const add = group(t.add.heading);
  // Refilled from the answer rather than from a copy this file keeps:
  // normalised when the probe validated it, exactly as typed when it did not,
  // so a rejected address is still on screen to be corrected.
  const url = textInput(S.probe ? S.probe.baseUrl : "");
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

  return options.addOnly ? [add] : [list, add];
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

/** How often the About tab re-asks Rust where the update stands.
 *
 *  Only while that tab is open, and only in Tauri. The check itself runs once
 *  at launch and once per press of the button below -- this timer is a *read*,
 *  not a poll of the release server, and it exists because a download that
 *  takes thirty seconds must not leave the group frozen on "Downloading...".
 *  Rust never pushes: there is not one `emit` in the shell, and an updater is
 *  no reason to introduce the first. */
const UPDATE_POLL_MS = 1500;
let updateTimer = null;

/** Repaints the Updates group from one of the three `update_*` answers.
 *
 *  All three return the SAME shape the settings payload carries at
 *  `about.updates`, so this is a patch of one branch rather than a second
 *  rendering path. `null` means there was nobody to ask (a plain browser, the
 *  Playwright suite with no stub for this command) -- and then nothing is
 *  painted, because a state Rust never produced would be this file inventing
 *  one. */
async function callUpdate(command) {
  const updates = await callRust(command);
  if (!updates || !S.view) return;
  // Cheap identity check: this runs on a timer, and rebuilding the tab's DOM
  // every 1.5s would fight a click that is mid-press.
  if (JSON.stringify(S.view.about.updates) === JSON.stringify(updates)) return;
  S.view.about.updates = updates;
  if (S.tab === "about") renderBody();
}

function startUpdatePolling() {
  stopUpdatePolling();
  updateTimer = setInterval(() => {
    if (settingsOpen && S.tab === "about") callUpdate("update_status");
  }, UPDATE_POLL_MS);
}

function stopUpdatePolling() {
  if (updateTimer === null) return;
  clearInterval(updateTimer);
  updateTimer = null;
}

/** Render the small Markdown subset used in release notes as safe DOM nodes.
 * URLs remain selectable text, like About's other URLs; no HTML is interpreted
 * and unsupported Markdown remains readable text. */
function releaseNotes(markdown) {
  const notes = node("div", "update-notes");
  const inline = (parent, value) => {
    const tokens = /https?:\/\/[^\s<]+|`([^`\n]+)`|\*\*([^*\n]+)\*\*|(?<![\p{L}\p{N}_])__([^_\n]+)__(?![\p{L}\p{N}_])|\*([^*\n]+)\*|(?<![\p{L}\p{N}_])_([^_\n]+)_(?![\p{L}\p{N}_])|\[([^\[\]\n]+)\]\(([^()\n]+)\)/gu;
    let end = 0;
    for (const match of value.matchAll(tokens)) {
      parent.append(document.createTextNode(value.slice(end, match.index)));
      if (match[1]) parent.append(node("code", "", match[1]));
      else if (match[2] || match[3]) parent.append(node("strong", "", match[2] || match[3]));
      else if (match[4] || match[5]) parent.append(node("em", "", match[4] || match[5]));
      else parent.append(document.createTextNode(match[6] ? `${match[6]} (${match[7]})` : match[0]));
      end = match.index + match[0].length;
    }
    parent.append(document.createTextNode(value.slice(end)));
  };
  let paragraph = null, lists = [], fence = null, code = null, headingLevel = null;
  for (const line of markdown.replace(/\r\n?/g, "\n").split("\n")) {
    if (fence) {
      const close = line.trim();
      if (close.length >= fence.length && [...close].every(char => char === fence[0])) {
        fence = null;
      } else code.append(document.createTextNode((code.childNodes.length ? "\n" : "") + line));
      continue;
    }
    const fenced = line.match(/^\s{0,3}(`{3,}|~{3,})/);
    if (fenced) {
      paragraph = null;
      lists = [];
      fence = fenced[1];
      const pre = node("pre");
      code = node("code");
      pre.append(code);
      notes.append(pre);
      continue;
    }
    if (!line.trim()) { paragraph = null; continue; }
    const heading = line.match(/^\s{0,3}(#{1,6})\s+(.+?)\s*$/);
    if (heading) {
      // About and Updates already establish the page hierarchy.
      headingLevel ??= heading[1].length;
      const title = node(`h${Math.min(6, Math.max(3, 3 + heading[1].length - headingLevel))}`);
      inline(title, heading[2].replace(/\s+#+\s*$/, ""));
      notes.append(title);
      paragraph = null;
      lists = [];
      continue;
    }
    const item = line.match(/^(\s*)(?:([-+*])|(\d+)[.)])\s+(.+)$/);
    if (item) {
      const indent = item[1].replace(/\t/g, "    ").length, tag = item[3] ? "ol" : "ul";
      while (lists.length && (lists.at(-1).indent > indent ||
        (lists.at(-1).indent === indent && lists.at(-1).tag !== tag))) lists.pop();
      if (!lists.length || lists.at(-1).indent < indent) {
        const list = node(tag);
        if (tag === "ol") list.start = Number(item[3]);
        (lists.at(-1)?.item || notes).append(list);
        lists.push({indent, tag, list});
      }
      const entry = node("li");
      inline(entry, item[4]);
      lists.at(-1).list.append(entry);
      lists.at(-1).item = entry;
      paragraph = null;
      continue;
    }
    if (lists.length && /^\s+/.test(line)) {
      inline(lists.at(-1).item, " " + line.trim());
      continue;
    }
    lists = [];
    if (!paragraph) { paragraph = node("p"); notes.append(paragraph); }
    inline(paragraph, (paragraph.childNodes.length ? " " : "") + line.trim());
  }
  return notes;
}

/** The Updates group: one sentence, whatever notes came with the offer, and
 *  the buttons Rust said to draw.
 *
 *  Neither button is ever rendered disabled. Rust sends a label or `null`, and
 *  `null` means the control must not exist -- a greyed-out "Install" invites a
 *  click that does nothing, and both moments it is withheld (before the first
 *  check has settled, and while a download is running) are moments where the
 *  honest answer is "not yet", not "no". */
function updatesGroup(u) {
  const box = group(u.heading);
  box.dataset.group = "updates";
  const status = node("p", "update-status", u.status.text);
  status.tabIndex = 0;
  // Rust's colour, assigned through the CSSOM -- a `style=""` attribute is
  // blocked by `style-src 'self'`. Green/amber/red mean what they mean on the
  // cards, and this file does not choose between them.
  status.style.color = u.status.color;
  const notes = node("div", "update-notes-slot");
  if (u.notes) notes.tabIndex = 0;
  if (u.notes) notes.appendChild(releaseNotes(u.notes));

  const controls = [];
  if (u.installLabel) {
    const install = button(u.installLabel, "install-update");
    install.addEventListener("click", () => callUpdate("update_install"));
    controls.push(install);
  }
  if (u.checkLabel) {
    const check = button(u.checkLabel, "check-updates");
    check.addEventListener("click", () => callUpdate("update_check"));
    controls.push(check);
  }
  const actions = actionRow(...controls);
  actions.classList.add("update-controls");
  box.appendChild(actions);

  box.append(status, help(u.help), notes);
  return box;
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
  return [box, updatesGroup(t.updates)];
}

function advanced(...content) {
  const details = node("details", "connection-advanced");
  details.append(node("summary", null, S.view.connections.advancedLabel), ...content);
  return details;
}

// Navigation never serializes a draft or a password. Keep the current DOM
// alive until the operator either returns to it or explicitly discards it.
function navigate(next) {
  const dirty = unappliedEdits().length > 0 ||
    Array.from(document.querySelectorAll('#settings input[type="password"]')).some(el => el.value) ||
    // A key typed in a walk lives in `walk.secret` once its step is left, and
    // so does a value typed into one of its inputs.
    Boolean(S.walk?.secret) || walkInputsEdited(S.walk);
  if (!dirty) { next(); return; }
  const labels = S.view.connections;
  const box = $s("settingsConfirm");
  const heading = node("h2", "group-hdr", labels.unsavedHeading);
  const keep = button(labels.keepLabel, "primary");
  const discard = button(labels.discardLabel, "discard");
  const dismiss = () => { box.hidden = true; box.replaceChildren(); };
  keep.addEventListener("click", dismiss);
  discard.addEventListener("click", () => { dismiss(); next(); });
  box.replaceChildren(heading, help(labels.unsavedHelp), actionRow(keep, discard));
  box.hidden = false;
  box.setAttribute("role", "alert");
  keep.focus();
}

function showConnections() {
  S.tab = "connections";
  S.editor = null;
  S.walk = null;
  S.catalog = false;
  S.route = null;
  S.probe = null;
  S.search = {};
  S.status = "";
  render();
}

function openConnection(entry) {
  navigate(() => {
    S.tab = "connections";
    S.catalog = false;
    S.editor = { ...entry };
    S.walk = null;
    S.probe = null;
    S.search = {};
    S.status = "";
    render();
  });
}

function connectionButton(entry, cls) {
  const b = button("", cls);
  b.dataset.connection = entry.id || entry.kind;
  b.dataset.kind = entry.kind;
  const text = node("span", "connection-copy");
  text.append(node("span", "connection-title", entry.title));
  if (entry.identity) text.appendChild(node("span", "connection-identity", entry.identity));
  text.appendChild(node("span", "connection-feeds", entry.feeds));
  b.append(text);
  if (entry.status) {
    const state = node("span", "connection-state", entry.status.text);
    state.style.color = entry.status.color;
    b.appendChild(state);
  }
  b.addEventListener("click", () => openConnection(entry));
  return b;
}

function connectionsTab(t) {
  const matches = entry => !S.route?.kinds?.length || S.route.kinds.includes(entry.kind);
  const rows = t.rows.filter(matches);
  const heading = node("div", "connection-heading row");
  const title = node("div", "stack");
  title.append(node("h2", null, S.catalog ? t.catalogHeading : S.route?.heading || t.heading),
    help(S.catalog ? t.catalogHelp : S.route?.help || t.summary));
  heading.append(title, node("span", "grow"));
  if (S.route) {
    const all = button(t.allLabel, "all-connections");
    all.addEventListener("click", showConnections);
    heading.append(all);
  }
  if (S.catalog) {
    const back = button(t.backLabel, "connection-back");
    back.addEventListener("click", () => navigate(showConnections));
    const catalog = node("div", "connection-catalog");
    for (const entry of t.catalog.filter(matches)) {
      // Providers have one persisted configuration; the catalog opens that
      // same editor, while accounts/hosts/status pages can be added again.
      const existing = t.rows.find(row => row.kind === entry.kind && !row.entityId);
      catalog.appendChild(connectionButton(existing || entry, "connection-option"));
    }
    return [back, heading, catalog];
  }
  const add = button(t.addLabel, "primary connection-add");
  add.addEventListener("click", () => { S.catalog = true; render(); });
  heading.appendChild(add);
  const list = node("div", "connection-list");
  for (const entry of rows) list.appendChild(connectionButton(entry, "connection-row"));
  if (!rows.length) list.appendChild(help(t.empty));
  const page = [heading, list];
  if (S.view.accounts.unattributed.length && matches({kind:"account"})) {
    const orphaned = button(t.unattributedLabel, "connection-orphans");
    orphaned.addEventListener("click", () => openConnection({ kind: "unattributed", title: t.unattributedLabel }));
    page.push(orphaned);
  }
  page.push(help(t.layoutHelp));
  return page;
}

// MARK: a source's steps

/** The first draft of a source's steps, from what is on now. A source with one
 *  part has nothing to ask, so that part starts picked. */
function startWalk(entry) {
  const { parts } = entry.walk;
  // What each part's inputs hold now, so leaving can tell an edit from a
  // reopened step, and a value is shown before it is ever sent.
  const inputs = Object.fromEntries(
    parts.flatMap(part => (part.inputs || []).map(input => [input.id, input.value])),
  );
  // A source whose parts are all set up together (GitHub's token and
  // organizations) has nothing to pick: the alternative to Finish is Remove.
  const fixed = entry.walk.pick === false;
  return {
    kind: entry.kind,
    // The row these steps are for, so a draft is never carried onto another
    // account's steps: `null` for a source with one configuration and for an
    // account that has no row until Finish.
    entity: entry.entityId ?? null,
    pick: !fixed,
    picked: new Set(parts.filter(part => fixed || part.on || parts.length === 1).map(part => part.id)),
    // Rust says so when a source can exist with every part off (Sentry's token
    // alone); otherwise a source is added when one of its parts is on.
    added: entry.walk.added ?? parts.some(part => part.on),
    index: 0,
    secret: "",
    inputs,
    inputsStart: { ...inputs },
    addTile: true,
    problem: "",
  };
}

/** Whether `part` asks for nothing an earlier picked part has not already asked:
 *  it has a key or typed values, and every one of them is on an earlier step.
 *  Sentry's two parts share one token and one slug, so they are asked once,
 *  on whichever part is picked first. A part with neither (a plan opt-in) is
 *  never covered, because its step is the consent itself. */
function walkCovered(part, earlier) {
  const inputs = part.inputs || [];
  if (!part.secret && !inputs.length) return false;
  return (!part.secret || earlier.some(item => item.secret)) &&
    inputs.every(input => earlier.some(item => (item.inputs || []).some(seen => seen.id === input.id)));
}

/** What is asked, in order: which parts (when there is a choice), one step per
 *  part picked that has something left to ask, then Ready. A tick changes this
 *  list, so it is derived rather than stored. */
function walkSteps(parts, walk) {
  const picked = pickedParts(parts, walk);
  return [
    ...(parts.length > 1 && walk.pick ? ["pick"] : []),
    ...picked.filter((part, index) => !walkCovered(part, picked.slice(0, index))).map(part => part.id),
    "ready",
  ];
}

const pickedParts = (parts, walk) => parts.filter(part => walk.picked.has(part.id));

/** Whether a typed value differs from what the steps opened with. */
const walkInputsEdited = walk =>
  Boolean(walk) && Object.keys(walk.inputs).some(id => walk.inputs[id] !== walk.inputsStart[id]);

/** One typed, non-secret value. It lives in the draft, so it survives Back and
 *  Next, and goes to Rust only at Finish. */
function walkInput(item, walk, onEnter) {
  const input = item.number ? numberInput(walk.inputs[item.id], 0) : textInput(walk.inputs[item.id]);
  // Rates are fractional; the default step of 1 would flag 0.106 invalid.
  if (item.number) input.step = "any";
  input.addEventListener("input", () => { walk.inputs[item.id] = input.value; });
  input.addEventListener("keydown", event => { if (event.key === "Enter") onEnter(); });
  return field(`walk-${item.id}`, item.label, input);
}

function walkPickStep(parts, walk, onChange) {
  const list = node("div", "walk-picks");
  for (const part of parts) {
    const row = node("label", "walk-pick");
    row.dataset.part = part.id;
    const box = checkbox(walk.picked.has(part.id));
    box.addEventListener("change", () => {
      if (box.checked) walk.picked.add(part.id);
      else walk.picked.delete(part.id);
      onChange();
    });
    const copy = node("span", "walk-pick-copy");
    copy.append(node("span", "walk-pick-title", part.title), node("span", "walk-pick-summary", part.summary));
    row.append(box, copy);
    list.appendChild(row);
  }
  return [list];
}

/** One part. Its status only ever comes from a source that is already on: a
 *  first walk has read nothing, because reading is what Finish consents to. */
function walkPartStep(part, walk, labels, onEnter) {
  const content = [help(part.help)];
  if (part.on && part.status) content.push(node("p", "help walk-status", part.status));
  if (part.secret) {
    const input = textInput("", "password");
    input.value = walk.secret;
    input.placeholder = part.secret.placeholder;
    input.dataset.walkFocus = "true";
    input.addEventListener("input", () => { walk.secret = input.value; });
    input.addEventListener("keydown", event => { if (event.key === "Enter") onEnter(); });
    content.push(field("walk-secret", part.secret.label, input));
    if (part.secret.stored) content.push(node("p", "help walk-stored", part.secret.storedLabel));
    if (walk.problem) content.push(node("p", "help walk-problem", labels.keyRequired));
  }
  const inputs = part.inputs || [];
  for (const item of inputs.filter(item => !item.advanced)) content.push(walkInput(item, walk, onEnter));
  if (part.inputsHelp) content.push(help(part.inputsHelp));
  const extra = inputs.filter(item => item.advanced);
  if (extra.length) {
    content.push(advanced(
      ...extra.map(item => walkInput(item, walk, onEnter)),
      ...(part.advancedHelp ? [help(part.advancedHelp)] : []),
    ));
  }
  return content;
}

/** The tiles Ready offers: one for each picked part that has none, by tile
 *  source. A part may name its own tile; the rest use the source's. */
function walkTileOffers(parts, tile, walk) {
  const offers = [];
  for (const part of pickedParts(parts, walk)) {
    const offer = part.tile || tile;
    if (!offer.present && !offers.some(seen => seen.source === offer.source)) offers.push(offer);
  }
  return offers;
}

function walkReadyStep(parts, tile, walk, labels, removeHelp) {
  if (!walk.picked.size) return [help(removeHelp || labels.removeHelp)];
  const summary = node("ul", "walk-summary");
  for (const part of parts) {
    const on = walk.picked.has(part.id);
    summary.appendChild(node("li", on ? null : "walk-off",
      on ? `\u2713 ${part.ready}` : `\u2014 ${part.ready}: ${labels.notSetUp}`));
  }
  const content = [help(labels.readyHelp), summary];
  const offers = walkTileOffers(parts, tile, walk);
  if (offers.length) {
    const offer = node("label", "walk-tile");
    const box = checkbox(walk.addTile);
    box.addEventListener("change", () => { walk.addTile = box.checked; });
    // One tick covers every tile offered; the sentence names each.
    const text = offers.length === 1
      ? offers[0].label
      : `Add ${offers.map(item => item.noun).join(" and ")}`;
    offer.append(box, node("span", null, text));
    content.push(offer);
  }
  return content;
}

/** Everything chosen goes to Rust in one call, and only here. A refusal leaves
 *  the steps open with the choices and the key intact. */
async function finishWalk(entry, control) {
  const walk = S.walk;
  const { parts } = entry.walk;
  control.disabled = true;
  let result;
  const picked = pickedParts(parts, walk);
  const previous = new Set(S.view.connections.rows.map(row => row.id));
  // Only a picked part's inputs are sent; an unpicked part's are forgotten.
  const inputs = Object.fromEntries(
    picked.flatMap(part => (part.inputs || []).map(input => [input.id, walk.inputs[input.id]])),
  );
  try {
    result = await callRust("settings_save_source", {
      kind: walk.kind,
      // The account these steps are for; absent from a source with one
      // configuration, and null for an account Finish creates.
      ...("row" in entry.walk ? { row: entry.walk.row } : {}),
      parts: picked.map(part => part.id),
      secret: picked.some(part => part.secret) && walk.secret ? walk.secret : null,
      // Left out for a source with none, so its call is the one it always was.
      ...(Object.keys(inputs).length ? { inputs } : {}),
      addTile: walk.addTile,
    });
  } catch {
    control.disabled = false;
    S.status = S.view.connections.saveFailed;
    $s("settingsStatus").textContent = S.status;
    return;
  }
  if (result?.saved) {
    S.walk = null;
    S.editor = null;
    S.catalog = false;
    // An account that is still there lands on its card, where its repos are
    // tracked; a removal lands on the list.
    if (picked.length && "row" in entry.walk) {
      const rows = result.settings.connections.rows;
      const saved = walk.entity
        ? rows.find(row => row.entityId === walk.entity)
        : rows.find(row => row.kind === walk.kind && !previous.has(row.id));
      if (saved) S.editor = { ...saved, walk: undefined, card: true };
    }
  }
  apply(result);
}

/** The steps of a source that has any. A source with one configuration has
 *  its steps on the catalog entry, which its row and Add source both open; a
 *  source with rows (GitHub) carries its own `walk` on each row, filled from
 *  that row, and Add source opens the catalog's blank one. */
function walkEditor(entry) {
  const labels = S.view.connections.walkLabels;
  const { parts, tile } = entry.walk;
  // One draft per row: steps opened on another account start from that one.
  if (S.walk && S.walk.entity !== (entry.entityId ?? null)) S.walk = null;
  S.walk ||= startWalk(entry);
  const walk = S.walk;
  const steps = walkSteps(parts, walk);
  const step = steps[walk.index];
  const part = parts.find(item => item.id === step);

  const progress = node("p", "walk-step");
  const paintProgress = () => {
    progress.textContent = labels.step
      .replace("{title}", entry.title)
      .replace("{n}", walk.index + 1)
      .replace("{total}", walkSteps(parts, walk).length);
  };
  paintProgress();

  const advance = () => {
    if (part?.secret && !part.secret.stored && !walk.secret.trim()) {
      walk.problem = labels.keyRequired;
      render();
      return;
    }
    walk.problem = "";
    walk.index += 1;
    render();
  };

  const forward = step === "ready"
    ? button(labels.finishLabel, "primary walk-finish")
    : button(labels.nextLabel, "primary walk-next");
  forward.dataset.walkFocus = "true";
  if (step === "ready") forward.addEventListener("click", () => finishWalk(entry, forward));
  else forward.addEventListener("click", advance);
  // An added source may have everything unticked: that is how it is removed.
  const allowForward = () => { forward.disabled = step === "pick" && !walk.picked.size && !walk.added; };
  allowForward();

  const heading = step === "pick" ? labels.pickHeading
    : step !== "ready" ? part.title
    : walk.picked.size ? labels.readyHeading : labels.removeHeading;
  const content = step === "pick"
    ? walkPickStep(parts, walk, () => { paintProgress(); allowForward(); })
    : step === "ready" ? walkReadyStep(parts, tile, walk, labels, entry.walk.removeHelp)
    : walkPartStep(part, walk, labels, advance);

  const cancel = button(labels.cancelLabel, "walk-cancel");
  cancel.addEventListener("click", showConnections);
  const actions = node("div", "row walk-actions");
  actions.appendChild(cancel);
  // A one-part source's steps are just Ready once Remove clears the picks.
  if (walk.added && walk.index === 0 && step !== "ready") {
    const remove = button(labels.removeLabel, "discard walk-remove");
    remove.addEventListener("click", () => {
      walk.picked.clear();
      walk.secret = "";
      walk.problem = "";
      walk.index = walkSteps(parts, walk).length - 1;
      render();
    });
    actions.appendChild(remove);
  }
  // An existing account's repos are tracked on its card, not in these steps.
  if (entry.walk.cardLabel && walk.index === 0 && step !== "ready") {
    const card = button(entry.walk.cardLabel, "walk-card");
    card.addEventListener("click", () => openConnection({ ...entry, walk: undefined, card: true }));
    actions.appendChild(card);
  }
  actions.appendChild(node("span", "grow"));
  if (walk.index > 0) {
    const back = button(labels.backLabel, "walk-back");
    back.addEventListener("click", () => {
      walk.problem = "";
      walk.index -= 1;
      render();
    });
    actions.appendChild(back);
  }
  actions.appendChild(forward);

  const page = node("div", "walk");
  page.dataset.step = step;
  page.append(progress, node("h2", null, heading), ...content, actions);
  // A render replaces the page, so keyboard focus would fall back to the top.
  // Only if nothing in the new page has taken it since: the frame can land after
  // the operator (or a fast test) has already clicked into another field, and
  // moving focus then sends what they are typing into the wrong input.
  requestAnimationFrame(() => {
    if (page.contains(document.activeElement)) return;
    page.querySelector("[data-walk-focus]")?.focus();
  });
  return [page];
}

function connectionEditor() {
  const t = S.view.connections;
  const entry = S.editor;
  const current = entry.id ? t.rows.find(row => row.id === entry.id) : null;
  // A removal returns a new persisted list; it cannot strand a stale editor.
  if (entry.entityId && !current) { S.editor = null; return connectionsTab(t); }
  // An account's card is where its repos are tracked; every other editor of a
  // source with steps is the steps. A row that carries its own steps (an
  // account's) opens those, and a source with one configuration opens the
  // catalog's.
  const stepped = entry.card ? null
    : current?.walk ? current
    : t.catalog.find(item => item.kind === entry.kind && item.walk);
  if (stepped) return walkEditor(stepped);
  const back = button(t.backLabel, "connection-back");
  back.addEventListener("click", () => navigate(showConnections));
  const heading = node("div", "connection-heading row");
  const title = node("div", "stack");
  title.append(node("h2", null, current?.title || entry.title),
    help(current?.feeds || entry.feeds));
  heading.append(title, node("span", "grow"));
  if (current?.status) {
    const status = node("span", "connection-state", current.status.text);
    status.style.color = current.status.color;
    heading.appendChild(status);
  }
  let content = [];
  if (entry.kind === "account") {
    const accounts = S.view.accounts;
    content = accountsTab({ ...accounts, rows: accounts.rows.filter(a => a.id === entry.entityId), unattributed: [] });
  } else if (entry.kind === "host") {
    const hosts = S.view.hosts;
    content = hostsTab({ ...hosts, rows: hosts.rows.filter(h => h.id === entry.entityId) },
      { edit: !!entry.entityId, addOnly: !entry.entityId });
  } else if (entry.kind === "openclaw") content = openclawTab(S.view.openclaw);
  else if (entry.kind === "vendor") {
    const services = S.view.services;
    content = servicesTab({ ...services, rows: services.rows.filter(v => v.id === entry.entityId) },
      { edit: !!entry.entityId, addOnly: !entry.entityId });
  } else if (entry.kind === "unattributed") {
    const accounts = S.view.accounts;
    const orphaned = group(accounts.unattributedHeading);
    for (const repo of accounts.unattributed) orphaned.appendChild(repoRow(accounts, null, repo));
    orphaned.appendChild(help(accounts.unattributedHelp));
    content = [orphaned];
  }
  return [back, heading, ...content];
}

function renderBody() {
  const body = $s("settingsBody");
  body.dataset.tab = S.tab;
  if (S.tab === "connections") {
    body.replaceChildren(...(S.editor ? connectionEditor() : connectionsTab(S.view.connections)));
    return;
  }
  const build = {
    general: generalTab,
    accounts: accountsTab,
    hosts: hostsTab,
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

async function openSettings(tab) {
  const route = tab && typeof tab === "object" && typeof tab.source === "string" ? tab : null;
  const chooseRoute = () => {
    S.tab = S.view.tabs.some(t => t.id === tab) ? tab : "connections";
    S.editor = null;
    S.walk = null;
    S.catalog = false;
    S.route = route ? S.view.connectionRoute : null;
    // Aggregate tiles don't identify one account/host/provider to edit. Their
    // links land on Sources so the operator can choose the right one.
    const kinds = { azure: "azure", openclaw: "openclaw", services: "services" };
    const kind = kinds[tab];
    if (kind) {
      const entry = S.view.connections.rows.find(r => r.kind === kind) ||
        S.view.connections.catalog.find(r => r.kind === kind);
      if (entry) S.editor = { ...entry };
    }
    const target = S.route?.editor;
    if (target) {
      const entry = [...S.view.connections.rows, ...S.view.connections.catalog]
        .find(item => item.kind === target.kind && (!target.entityId || item.entityId === target.entityId));
      if (entry) S.editor = { ...entry, ...(target.kind === "account" ? { walk: undefined, card: true } : {}) };
      if (target.kind === "unattributed") S.editor = { kind: target.kind, title: S.view.connections.unattributedLabel };
    }
    S.status = "";
    S.probe = null;
    S.pair = {};
    S.repair = new Set();
    S.unpaired = new Set();
    S.search = {};
    render();
  };
  if (settingsOpen) {
    navigate(async () => {
      if (route) S.view = await callRust("settings_view", { route }, "sample-settings.json");
      chooseRoute();
    });
    return;
  }
  // Offline (no Tauri), the same dumped-fixture path the cockpit uses, so the
  // surface can be opened in a plain browser and by the Playwright suite.
  const view = await callRust("settings_view", route ? { route } : {}, "sample-settings.json");
  if (!view) return;
  S.view = view;
  chooseRoute();
  S.status = "";
  // A probe answer must not outlive the session that ran it: reopening
  // Settings would otherwise show a component picker for an address nobody
  // just typed. The same goes for a certificate offered for trust.
  S.probe = null;
  S.pair = {};
  S.repair = new Set();
  S.unpaired = new Set();
  settingsOpen = true;
  // dashboard.js hides whichever view is on screen; this file does not know
  // which that is.
  document.dispatchEvent(new CustomEvent("solador:settings", { detail: true }));
  $s("settings").hidden = false;
  render();
  // The group renders from the payload above; this keeps it current while a
  // download runs. Stopped again on close, so a cockpit nobody is configuring
  // has no timer of its own.
  startUpdatePolling();
}

async function finishCloseSettings() {
  settingsOpen = false;
  stopUpdatePolling();
  $s("settings").hidden = true;
  // Closing must remove a discarded password, not merely hide its input.
  S.walk = null;
  $s("settingsBody").replaceChildren();
  $s("settingsConfirm").replaceChildren();
  $s("settingsConfirm").hidden = true;
  // ...and puts it back.
  document.dispatchEvent(new CustomEvent("solador:settings", { detail: false }));
  // Repaint at the real width now rather than up to a poll interval late: the
  // cockpit measured zero while it was hidden.
  await refreshCockpit();
  // ...and the panels, which keep their own timers. Without this a credential
  // or address saved a second ago leaves its panel still displaying the setup
  // instruction that asked for it.
  await refreshPanels();
}

function closeSettings() {
  navigate(finishCloseSettings);
}

// The toolbar's Settings button is the overview's (dashboard.js builds it), so
// it is found by its action rather than bound at load.
document.addEventListener("click", (event) => {
  if (event.target.closest('.db-chrome [data-action="settings"]')) openSettings();
});
$s("settingsClose").addEventListener("click", closeSettings);

window.soladorSettings = { open: openSettings };

// Test-only introspection, matching app.js's `window.__SOLADOR_TEST__`:
// read-only, and no production behaviour depends on it.
window.__SOLADOR_SETTINGS_TEST__ = { open: openSettings, close: closeSettings, tab: () => S.tab };

})();
