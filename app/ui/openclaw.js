// The OpenClaw panel: per-agent status, cron health, channel connectivity and
// token usage for an OpenClaw agent farm. Same discipline as every other panel
// here — every string, count, abbreviation and colour arrives from Rust
// (`app/src-tauri/src/openclaw.rs`), and this file does layout and nothing else.
//
// Two things this file deliberately does NOT do:
//
// It does not decide what a status dot means. `dot.color` and `dot.opacity`
// arrive together, because `unknown` and `disabled` are the same muted colour
// and only the opacity tells them apart — re-deriving either from a status word
// here would put that distinction on the far side of the IPC boundary where no
// Rust test can see it.
//
// It does not build the approve command. `pairing.command` is the literal line
// an operator pastes into a shell; assembling it from a request id here would be
// a second implementation of a string whose whole value is being exactly right.
// It is rendered selectable for the same reason.
//
// Nothing below uses `innerHTML`. Agent names, model refs, channel names and
// cron error text all come from a remote gateway, and a webview parses markup —
// so rows are built with createElement + textContent and reach the DOM as text.
//
// Wrapped in an IIFE, which is load-bearing: classic scripts share one global
// scope, so a top-level `render()` here would silently replace app.js's and
// every host card would stop painting.
(function () {

/** How often the panel asks Rust for a fresh payload.
 *
 *  This is a *read* cadence, not a poll cadence — nothing here drives the
 *  gateway. The session behind this panel is event-driven: Rust holds a
 *  WebSocket open and rewrites its state as frames land. This interval only
 *  decides how soon the window notices, and it is cheap: one lock and one JSON
 *  build. */
const REFRESH_MS = 2000;

const $o = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/** A status dot. Both the colour and the opacity are Rust's decision. */
function dotNode(dot, extraClass) {
  const el = node("span", extraClass ? "dot " + extraClass : "dot");
  if (!dot) return el;
  el.style.background = dot.color;
  // Set even at 1, so a dot that should be dimmed can never inherit a stale
  // value from a reused node.
  el.style.opacity = String(dot.opacity);
  return el;
}

/** A `{text, color}` line. */
function textNode(tag, cls, line) {
  const el = node(tag, cls, line.text);
  el.style.color = line.color;
  return el;
}

function sectionHeader(section) {
  return textNode("div", "oc-hdr", { text: section.header, color: section.headerColor });
}

/** One agent: a name line (dot, optional emoji, name, `running` badge) with the
 *  model ref on its own line beneath it.
 *
 *  Two lines rather than one because the alternative was truncation: at a
 *  quarter-width card the name and the model ref competed for ~300pt and both
 *  lost characters. Underneath, each gets the full width of the card, and the
 *  panel's `min_width` is set by the name line alone — which is what lets
 *  OpenClaw sit in a quarter beside a three-quarter Containers.
 *
 *  The extra line is free where this panel actually lives: it shares a row with
 *  Containers and is the shorter card, so `align-items:stretch` was padding
 *  that space out anyway. If OpenClaw ever becomes the tallest panel in its
 *  row, this is the first thing to revisit. */
function agentNode(agent) {
  const box = node("div", "oc-agent");
  const row = node("div", "oc-row");
  row.append(dotNode(agent.dot));
  // Absent, not empty: an agent without an emoji must not reserve the gap one
  // would have taken, or the names stop lining up.
  if (agent.emoji) row.appendChild(node("span", "oc-emoji", agent.emoji));

  const name = node("span", "oc-name", agent.name);
  name.style.color = agent.nameColor;
  row.appendChild(name);

  row.appendChild(node("span", "grow"));
  if (agent.trailing) {
    const trailing = node("span", "oc-badge", agent.trailing);
    trailing.style.color = agent.trailingColor;
    row.appendChild(trailing);
  }
  box.appendChild(row);

  if (agent.detail) {
    const detail = node("div", "oc-detail", agent.detail);
    detail.style.color = agent.detailColor;
    box.appendChild(detail);
  }
  return box;
}

function cronNode(cron) {
  const box = node("div", "oc-section");
  box.dataset.section = "cron";
  box.appendChild(sectionHeader(cron));

  const row = node("div", "oc-row");
  const summary = node("span", "oc-summary", cron.summary);
  summary.style.color = cron.summaryColor;
  row.append(dotNode(cron.dot), summary, node("span", "grow"));
  box.appendChild(row);

  // Only when a job actually failed. A reserved empty line would be a row that
  // means "no error" and looks like one that means "error unavailable".
  if (cron.error) box.appendChild(textNode("p", "oc-error", cron.error));
  return box;
}

/** Channel dots, wrapping to the next line when the row runs out of width. */
function channelsNode(channels) {
  const box = node("div", "oc-section");
  box.dataset.section = "channels";
  box.appendChild(sectionHeader(channels));

  const flow = node("div", "oc-flow");
  for (const channel of channels.rows || []) {
    const chip = node("span", "oc-chip");
    const name = node("span", "oc-channel", channel.name);
    name.style.color = channel.nameColor;
    chip.append(dotNode(channel.dot), name);
    flow.appendChild(chip);
  }
  box.appendChild(flow);
  return box;
}

/**
 * The pairing banner — the one part of this panel a human must act on.
 *
 * The dot pulses (`blink`, the same class and the same reduced-motion opt-out
 * the Repos panel's needs-approval dot uses) because no retry clears this: only
 * someone running the command below does.
 */
function pairingNode(pairing) {
  const box = node("div", "oc-pairing");

  const head = node("div", "oc-row");
  const title = node("span", "oc-pairing-title", pairing.title);
  title.style.color = pairing.titleColor;
  const dot = node("span", pairing.blinking ? "dot blink" : "dot");
  dot.style.background = pairing.dotColor;
  head.append(dot, title, node("span", "grow"));
  box.appendChild(head);

  if (pairing.command) {
    const command = node("code", "oc-command", pairing.command);
    command.style.color = pairing.commandColor;
    box.appendChild(command);
  }

  const device = node("span", "oc-device", pairing.device);
  device.style.color = pairing.deviceColor;
  box.appendChild(device);
  return box;
}

/** One runtime's block: the banner (at most one), then its data sections. */
function runtimeNode(runtime) {
  const box = node("div", "oc-runtime");
  box.dataset.runtime = runtime.id;

  // Only when a second runtime exists — Rust sends null otherwise.
  if (runtime.heading) box.appendChild(textNode("div", "oc-runtime-hdr", runtime.heading));

  if (runtime.pairing) box.appendChild(pairingNode(runtime.pairing));
  if (runtime.hint) box.appendChild(textNode("p", "oc-message", runtime.hint));
  if (runtime.connection) {
    const row = node("div", "oc-row");
    const dot = node("span", "dot");
    dot.style.background = runtime.connection.dotColor;
    const text = node("span", "oc-connection", runtime.connection.text);
    text.style.color = runtime.connection.color;
    row.append(dot, text, node("span", "grow"));
    box.appendChild(row);
  }

  if (runtime.agents) {
    const agents = node("div", "oc-section");
    agents.dataset.section = "agents";
    agents.appendChild(sectionHeader(runtime.agents));
    for (const agent of runtime.agents.rows || []) agents.appendChild(agentNode(agent));
    box.appendChild(agents);
  }
  if (runtime.cron) box.appendChild(cronNode(runtime.cron));
  if (runtime.channels) box.appendChild(channelsNode(runtime.channels));
  if (runtime.usage) box.appendChild(textNode("p", "oc-usage", runtime.usage));

  return box;
}

function renderOpenClaw(payload) {
  $o("openclawTitle").textContent = payload.title;
  $o("openclawTrailing").textContent = payload.trailing || "";

  const children = [];
  // "no agent runtime configured" — a sentence in a colour Rust chose. There is
  // no runtime block to render alongside it.
  if (payload.message) children.push(textNode("p", "oc-message", payload.message));
  for (const runtime of payload.runtimes || []) children.push(runtimeNode(runtime));

  $o("openclawBody").replaceChildren(...children);
  $o("openclawPanel").hidden = false;
}

async function refresh() {
  try {
    const payload = await callRust("openclaw", {}, "sample-openclaw.json");
    if (payload) renderOpenClaw(payload);
  } catch {
    // A failed read leaves the last good panel on screen. Rust owns the
    // connection state, and blanking the DOM here would report a socket
    // problem that is really an IPC one.
  }
}

refresh();
if (window.__TAURI__) {
  setInterval(() => { if (!settingsOpen) refresh(); }, REFRESH_MS);
}

// Brought current when Settings closes rather than waiting out the interval
// above — this panel's gateway URL and bearer token are both edited there.
registerPanelRefresh(refresh);

// Test-only introspection, matching app.js's `window.__SOLADOR_TEST__`:
// read-only, and no production behaviour depends on it.
window.__SOLADOR_OPENCLAW_TEST__ = { render: renderOpenClaw, refresh };

})();
