/**
 * Calculate pixel coordinates for a character offset in a textarea using the mirror div technique.
 * Creates an offscreen div with identical styling, inserts text up to the position,
 * appends a marker span, and measures its position.
 *
 * The mirror div is cached and reused across calls for performance.
 */

const MIRROR_DIV_ID = '__caret-mirror-div';

const PROPERTIES = [
  'direction', 'boxSizing', 'width',
  'overflowX', 'overflowY',
  'borderTopWidth', 'borderRightWidth', 'borderBottomWidth', 'borderLeftWidth',
  'borderStyle',
  'paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft',
  'fontStyle', 'fontVariant', 'fontWeight', 'fontStretch', 'fontSize',
  'fontSizeAdjust', 'lineHeight', 'fontFamily',
  'textAlign', 'textTransform', 'textIndent', 'textDecoration',
  'letterSpacing', 'wordSpacing', 'tabSize',
  'whiteSpace', 'wordWrap', 'wordBreak',
] as const;

let cachedDiv: HTMLDivElement | null = null;
let cachedSpan: HTMLSpanElement | null = null;

export function getCaretCoordinates(
  element: HTMLTextAreaElement,
  position: number
): { top: number; left: number; height: number } {
  // Clamp position to valid range
  const value = element.value;
  const pos = Math.max(0, Math.min(position, value.length));

  // Bail out for zero-width textareas
  if (element.offsetWidth === 0) {
    return { top: 0, left: 0, height: 0 };
  }

  // Reuse or create the mirror div
  if (!cachedDiv || !cachedDiv.isConnected) {
    cachedDiv = document.createElement('div');
    cachedDiv.id = MIRROR_DIV_ID;
    cachedSpan = document.createElement('span');
    document.body.appendChild(cachedDiv);
  }
  if (!cachedSpan) {
    cachedSpan = document.createElement('span');
  }

  const div = cachedDiv;
  const span = cachedSpan;
  const style = getComputedStyle(element);

  div.style.position = 'absolute';
  div.style.visibility = 'hidden';
  div.style.whiteSpace = 'pre-wrap';
  div.style.overflowWrap = 'break-word';

  for (const prop of PROPERTIES) {
    (div.style as any)[prop] = (style as any)[prop];
  }

  div.style.width = style.width;
  div.style.height = 'auto';
  div.style.overflow = 'hidden';

  // Clear all children before rebuilding content
  div.textContent = '';

  // Use createTextNode for the pre-cursor text so it coexists with the span
  const textBefore = document.createTextNode(value.substring(0, pos));
  div.appendChild(textBefore);

  // Append marker span for measurement
  span.textContent = value.substring(pos) || '.';
  div.appendChild(span);

  const lineHeight = parseInt(style.lineHeight) || parseInt(style.fontSize) * 1.7;

  return {
    top: span.offsetTop - element.scrollTop,
    left: span.offsetLeft - element.scrollLeft,
    height: lineHeight,
  };
}
