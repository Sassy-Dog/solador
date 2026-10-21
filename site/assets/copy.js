// Copy buttons for the blocks of text a visitor is meant to paste elsewhere.
//
// A `[data-copy]` button names the element whose text it copies, and ships
// `hidden`: with JavaScript off, or in a browser with no clipboard API, there
// is nothing for it to do, and the text is still there to select by hand. This
// script shows the button only where it can work. A copy that fails anyway (a
// permission refused, an insecure context) says so and selects the text,
// rather than claiming success.

const RESET_MS = 2000;

/** Selects all of an element's text, the fallback when the clipboard refuses. */
function selectText(element) {
  const range = document.createRange();
  range.selectNodeContents(element);
  const selection = window.getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
}

function wire(button) {
  const id = button.dataset.copy;
  const source = document.getElementById(id);
  if (!source) return;
  const status = document.querySelector(`[data-copy-status="${id}"]`);
  const label = button.textContent;
  let timer;

  const say = (text) => {
    if (status) status.textContent = text;
  };

  button.addEventListener("click", async () => {
    clearTimeout(timer);
    try {
      await navigator.clipboard.writeText(source.textContent);
      button.textContent = "Copied";
      say("Copied to the clipboard.");
    } catch {
      selectText(source);
      button.textContent = label;
      say("Could not copy. The text is selected: copy it with ⌘C or Ctrl+C.");
      return;
    }
    timer = setTimeout(() => {
      button.textContent = label;
      say("");
    }, RESET_MS);
  });
  button.hidden = false;
}

if (navigator.clipboard?.writeText) {
  document.querySelectorAll("button[data-copy]").forEach(wire);
}
