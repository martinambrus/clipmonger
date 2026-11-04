const clamp = (value, low, high) => Math.min(Math.max(value, low), high);
/** Top-left for a `size` popup: at the pointer (clamped inside `area`) or
 * centered in `area`. */
export function placePopup(mode, pointer, area, size) {
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
