/**
 * Where a drop the OS reported landed on the page (PLAN 7.15).
 *
 * Converts the OS's physical pixels to CSS pixels, shared by every drop target.
 * Call it before changing any state: a re-render first can move the target.
 */
export function elementAtDrop(physicalX: number, physicalY: number): Element | null {
  const scale = window.devicePixelRatio || 1;
  return document.elementFromPoint(physicalX / scale, physicalY / scale);
}
