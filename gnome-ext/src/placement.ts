// Pure popup placement: mirrors crates/app-ui/src/placement.rs `place`.
// No gi:// imports.
export type Placement = 'cursor' | 'center';
export interface Area { x: number; y: number; width: number; height: number }

const clamp = (value: number, low: number, high: number) => Math.min(Math.max(value, low), high);

/** Height share of the work area a popup opened near the bottom edge keeps
 * (mirrors CURSOR_MIN_HEIGHT_SHARE in crates/app-ui/src/placement.rs). */
const CURSOR_MIN_HEIGHT_SHARE = 0.4;

/** Height of a popup asking for `height`: at the pointer it stops at the
 * bottom of `area` unless that leaves too little room to be useful. */
export function popupHeight(mode: Placement, pointer: [number, number], area: Area, height: number): number {
  if (mode !== 'cursor') return height;
  const room = area.y + area.height - pointer[1];
  return Math.min(height, Math.max(room, Math.ceil(area.height * CURSOR_MIN_HEIGHT_SHARE)));
}

/** Top-left for a `size` popup: at the pointer (clamped inside `area`) or
 * centered in `area`. */
export function placePopup(mode: Placement, pointer: [number, number], area: Area, size: [number, number]): [number, number] {
  const [w, h] = size;
  if (mode === 'center') {
    return [
      w >= area.width ? area.x : area.x + Math.floor((area.width - w) / 2),
      h >= area.height ? area.y : area.y + Math.floor((area.height - h) / 2),
    ];
  }
  const maxX = area.x + Math.max(area.width - w, 0);
  const maxY = area.y + Math.max(area.height - h, 0);
  return [clamp(pointer[0], area.x, maxX), clamp(pointer[1], area.y, maxY)];
}
