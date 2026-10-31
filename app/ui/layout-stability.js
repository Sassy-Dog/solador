// Fit content in the page scroll; retain space through temporary empty/error
// frames. Width and deliberate tile configuration changes release old space.
(() => {
  const selector = '.db-tile-content, .panel-body, .card, .db-hidden-preview .db-preview-grid, .db-inspector[data-kind="details"] .db-inspector-body';
  const tracked = new Map();
  let frame = 0;
  let released = false;
  const configuration = element => {
    // A tile's Full view keeps no floor measured outside it: the key carries
    // the Full view's source and tile, so opening or switching one releases it,
    // in the mutation callback, before the next frame or any other task can measure it.
    const full = element.closest('#fullView');
    if (full) return `full|${full.dataset.fullSource || ''}|${full.dataset.fullTile || ''}|${element.dataset.detailState || ''}`;
    const tile = element.closest('.db-tile, .db-preview-tile') || element.querySelector('.db-preview-tile');
    return tile ? [tile.dataset.source, tile.dataset.presentation, tile.dataset.scope, tile.dataset.options].join('|') : element.dataset.detailState || '';
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
      const reset = released || snapshots.some(({state, width, key}) =>
        (state.width && Math.abs(state.width - width) > 1) || state.key !== key);
      released = false;
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
      const state = tracked.get(element);
      if (state) {
        // A changed key (a Full view opened or switched) drops the old floor now,
        // in this mutation callback, not in the next animation frame: a task that
        // measures before that frame would see the new view at the floor the old
        // key measured (#707). Closing releases nothing: the hidden body keeps its floor
        // for the same tile's next opening (#695), and the frame never measures it while
        // it is hidden (only the boot-time Full view, visible with no source, is measured
        // there). Only a release of a visible element resets the rest of the page.
        const key = configuration(element);
        const closing = element.closest('#fullView') && !element.closest('#fullView').dataset.fullSource;
        if (state.key !== key && !closing) {
          element.style.minHeight = '';
          state.height = 0;
          state.key = key;
          if (element.getClientRects().length) released = true;
        }
        return;
      }
      tracked.set(element, {width:0, height:0, key:configuration(element)});
      observer.observe(element, {box:'border-box'});
    });
    schedule();
  };
  new MutationObserver(discover).observe(document.body, {childList:true, subtree:true, attributes:true, attributeFilter:['data-detail-state', 'data-full-source', 'data-full-tile']});
  discover();
})();
