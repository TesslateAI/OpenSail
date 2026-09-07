/**
 * Keep the browser title on the VOIE product name.
 *
 * The vendored conversation renderer writes `DeepSeek Harness` into
 * `document.title` on mount. That package is byte-preserved; this interceptor
 * rewrites the assignment at the document property so every surface, including
 * session-title projections, stays VOIE-branded.
 */

const PRODUCT = "VOIE";
const LEGACY = /deepseek\s+harness/i;

let installed = false;

export function voieDocumentTitle(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.length === 0) return PRODUCT;
  const core = trimmed
    .replace(LEGACY, " ")
    .replace(/[\s—–−-]+/g, " ")
    .trim();
  if (core.length === 0 || core === PRODUCT) return PRODUCT;
  if (core.endsWith(PRODUCT)) {
    const withoutProduct = core.slice(0, -PRODUCT.length).trim();
    if (withoutProduct.length === 0) return PRODUCT;
    return `${withoutProduct} — ${PRODUCT}`;
  }
  return `${core} — ${PRODUCT}`;
}

/** Install once. Safe under Vite HMR. */
export function installVoieDocumentTitle(): void {
  if (installed) {
    document.title = voieDocumentTitle(document.title);
    return;
  }
  const describe = Object.getOwnPropertyDescriptor(Document.prototype, "title");
  if (describe?.set !== undefined && describe.get !== undefined) {
    const { get, set } = describe;
    Object.defineProperty(Document.prototype, "title", {
      configurable: true,
      enumerable: describe.enumerable ?? false,
      get() {
        return get.call(this);
      },
      set(value: string) {
        set.call(this, voieDocumentTitle(String(value ?? "")));
      },
    });
  }
  installed = true;
  document.title = voieDocumentTitle(document.title || PRODUCT);
}
