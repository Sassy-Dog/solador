// Column widths are part of the view model, never measured from live content.
// A narrow view scrolls; new digits and status words cannot move other cells.
function fixedColumns(table, widths) {
  const columns = document.createElement('colgroup');
  for (const width of widths) {
    const column = document.createElement('col');
    column.style.width = `${width}px`;
    columns.append(column);
  }
  table.style.minWidth = `${widths.reduce((total, width) => total + width, 0)}px`;
  table.append(columns);
}

// A runner's labels as chips, or Rust's fallback text when they were never
// recorded — never an empty cell, never a guessed label. Each chip's `kind` is
// Rust's: the label that put the runner in its group, one GitHub assigns, or
// the operator's own. Shared by Detail (dashboard.js) and Full (github.js).
function labelChips(chips, fallbackText) {
  const list = document.createElement('span');
  if (!chips) {
    list.className = 'db-label-none';
    list.textContent = fallbackText;
    return list;
  }
  list.className = 'db-label-chips';
  for (const chip of chips) {
    const item = document.createElement('span');
    item.className = 'db-label-chip';
    item.dataset.kind = chip.kind;
    item.textContent = chip.text;
    list.append(item);
  }
  return list;
}
