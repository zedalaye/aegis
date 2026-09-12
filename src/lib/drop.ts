/**
 * Where a drop the OS reported landed on the page (PLAN 7.15).
 *
 * The OS reports physical pixels from the top left of the webview; the DOM is
 * laid out in CSS pixels. Every drop target reads its row through this one
 * conversion, so the explorer and the project list cannot disagree about where
 * the same drop fell.
 *
 * Call it before changing any state. A re-render between the drop and the
 * lookup is a lookup against a different page from the one the drop was aimed
 * at — which is exactly how a hint line once moved the tree out from under it.
 */
export function elementAtDrop(physicalX: number, physicalY: number): Element | null {
  const scale = window.devicePixelRatio || 1;
  return document.elementFromPoint(physicalX / scale, physicalY / scale);
}
