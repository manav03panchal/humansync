/**
 * Calculate pixel coordinates for a character offset in a textarea using the mirror div technique.
 * Creates an offscreen div with identical styling, inserts text up to the position,
 * appends a marker span, and measures its position.
 */
export function getCaretCoordinates(
  element: HTMLTextAreaElement,
  position: number
): { top: number; left: number; height: number } {
  const div = document.createElement('div');
  const style = getComputedStyle(element);

  // Copy ALL relevant text-rendering styles from textarea to mirror div
  const properties = [
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

  div.style.position = 'absolute';
  div.style.visibility = 'hidden';
  div.style.whiteSpace = 'pre-wrap';
  div.style.wordWrap = 'break-word';

  for (const prop of properties) {
    (div.style as any)[prop] = (style as any)[prop];
  }

  // Use the textarea's actual width for accurate wrapping
  div.style.width = style.width;
  div.style.height = 'auto';
  div.style.overflow = 'hidden';

  // Insert text before cursor position
  div.textContent = element.value.substring(0, position);

  // Append marker span
  const span = document.createElement('span');
  span.textContent = element.value.substring(position) || '.';
  div.appendChild(span);

  document.body.appendChild(div);

  const lineHeight = parseInt(style.lineHeight) || parseInt(style.fontSize) * 1.7;

  const coordinates = {
    top: span.offsetTop - element.scrollTop,
    left: span.offsetLeft - element.scrollLeft,
    height: lineHeight,
  };

  document.body.removeChild(div);
  return coordinates;
}
