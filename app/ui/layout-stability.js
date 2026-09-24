// Fit content in the page scroll; retain space through temporary empty/error
// frames. Width and deliberate tile configuration changes release old space.
(() => {
  const selector = '.db-tile-content, .panel-body, .card, .db-hidden-preview .db-preview-grid, .db-inspector[data-kind="details"] .db-inspector-body';
  const tracked = new Map();
  let frame = 0;
  const configuration = element => {
    const tile = element.closest('.db-tile, .db-preview-tile') || element.querySelector('.db-preview-tile');
    return tile ? [tile.dataset.source, tile.dataset.presentation, tile.dataset.scope].join('|') : '';
  };
  const schedule = () => {
    if (frame) return;
    // Write outside ResizeObserver delivery to avoid WebKit feedback loops.
    frame = requestAnimationFrame(() => {
      frame = 0;
      const visible = [...tracked].filter(([element]) => element.getClientRects().length);
      const snapshots = visible.map(([element, state]) => ({element, state,
        width:element.getBoundingClientRect().width, key:configuration(element)}));
      // Clear every obsolete floor before measuring: stretched grid siblings
      // otherwise keep one another at the old row height.
      const reset = snapshots.some(({state, width, key}) =>
        (state.width && Math.abs(state.width - width) > 1) || state.key !== key);
      for (const {element, state, width, key} of snapshots) {
        if (reset) {
          element.style.minHeight = '';
          state.height = 0;
        }
        state.width = width;
        state.key = key;
      }
      const heights = snapshots.map(({element}) => element.getBoundingClientRect().height);
      snapshots.forEach(({element, state}, index) => {
        if (heights[index] > state.height) {
          state.height = heights[index];
          element.style.minHeight = `${state.height}px`;
        }
      });
    });
  };
  const observer = new ResizeObserver(schedule);
  const discover = () => {
    for (const element of tracked.keys()) {
      if (!element.isConnected) {
        observer.unobserve(element);
        tracked.delete(element);
      }
    }
    document.querySelectorAll(selector).forEach(element => {
      if (tracked.has(element)) return;
      tracked.set(element, {width:0, height:0, key:configuration(element)});
      observer.observe(element, {box:'border-box'});
    });
    schedule();
  };
  new MutationObserver(discover).observe(document.body, {childList:true, subtree:true});
  discover();
})();
