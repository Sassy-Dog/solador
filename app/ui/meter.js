// The one meter the overview's tiles and the Usage panel both paint: a label,
// a reading, and a track whose fill Rust sized and coloured. The Machines
// tile's CPU and RAM use it, and so does every plan limit, so a subscription's
// Week reads exactly as a machine's RAM does.
//
// Loaded before usage.js and dashboard.js, which both call it: classic
// scripts run in order, and `window.soladorDashboard` does not exist yet when
// usage.js first renders.
//
// Nothing is decided here. `fraction`, `color`, `pace` and `note` arrive from
// Rust; an unknown fraction is an empty, `aria-hidden` track, never a zero.
// Built with createElement + textContent, never markup.
(function () {

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/**
 * One metric: `{label, value, fraction, color, pace?, note?, title?, span?}`.
 *
 * Returns the metric element and its two parts the Machines tile adds to (the
 * acknowledgement badge goes in `reading`, a volume's Hide button before it in
 * `label`).
 *
 * `pace` is where the window stood when it was read, 0–1: a tick on the track,
 * so a fill past it is a burn faster than even. Decorative (`aria-hidden`):
 * the same fact is in `note`, which becomes the track's description, and,
 * with `showNote`, a visible line under it.
 */
function metricNode(m, rowLabel, { showNote = false } = {}) {
  const metric = el("div", m.span === "full" ? "db-metric db-metric-full" : "db-metric");
  const title = m.title || m.note;
  if (title) metric.title = title;
  const label = el("div", "db-stat-label");
  const reading = el("strong", "", m.value ?? "—");
  label.append(el("span", "", m.label), reading);
  const track = el("div", "db-meter");
  track.setAttribute("aria-label", `${rowLabel} ${m.label}`);
  if (m.note) track.setAttribute("aria-description", m.note);
  if (Number.isFinite(m.fraction)) {
    track.setAttribute("role", "meter");
    track.setAttribute("aria-valuemin", "0");
    track.setAttribute("aria-valuemax", "100");
    track.setAttribute("aria-valuenow", String(m.fraction * 100));
    track.setAttribute("aria-valuetext", m.value);
    const fill = el("span", "db-meter-fill");
    fill.style.width = `${m.fraction * 100}%`;
    fill.style.backgroundColor = m.color;
    track.append(fill);
    if (Number.isFinite(m.pace)) {
      const tick = el("span", "db-meter-pace");
      tick.setAttribute("aria-hidden", "true");
      tick.style.left = `${m.pace * 100}%`;
      track.append(tick);
    }
  } else track.setAttribute("aria-hidden", "true");
  metric.append(label, track);
  if (showNote && m.note) metric.append(el("p", "db-meter-note", m.note));
  return { metric, label, reading };
}

window.soladorMeter = { metricNode };

})();
