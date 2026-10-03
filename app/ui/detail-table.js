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
