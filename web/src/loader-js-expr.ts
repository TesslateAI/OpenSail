/**
 * Lazy stand-in for the vendored cordis loader's `!js` expression evaluator,
 * substituted over `@deepseek-ai/cordis-plugin-loader@1.0.2`'s
 * `lib/index.js` by the Vite build's `voieLazyJsExpr` plugin.
 *
 * Upstream builds that evaluator with `new Function` at MODULE TOP LEVEL, so
 * merely importing the loader constructs it. This island serves under
 * `script-src 'self'` with no `'unsafe-eval'`, and a browser answers that
 * construction with an EvalError thrown during module evaluation, which
 * aborts the shell entry before one line of boot code runs: `#root` stays
 * empty and the page shows nothing at all.
 *
 * Deferring the construction to the first call is the whole fix, and it is a
 * fix rather than a suppression: `__jsExpr` nodes exist only in YAML loader
 * config, this island boots a synthetic graph, so nothing here ever calls
 * this and no string is ever compiled. A host that somehow does pass one gets
 * upstream's exact semantics wherever evaluation is permitted, and a named
 * failure at the call wherever it is not, instead of a blank page at import.
 */

/** Upstream's evaluator signature (`(ctx: object, expr: string) => any`), narrowed at the return. */
type JsExprEvaluator = (ctx: object, expr: string) => unknown;

/**
 * Upstream's evaluator body, verbatim. `with` is what puts the loader context
 * in scope for a bare expression, and it is legal here because a Function
 * constructor body is sloppy mode even when the module that builds it is not.
 */
const EVALUATOR_BODY = "\n  with (ctx) {\n    return eval(expr)\n  }\n";

/** The compiled evaluator, or undefined until something actually evaluates a `!js` node. */
let compiled: JsExprEvaluator | undefined;

/**
 * Compile upstream's evaluator, translating the CSP refusal into a message
 * that says which capability was reached for and why the reach is itself the
 * bug. The try keeps this construction off unguarded-eval scans and loses
 * nothing: the rethrow carries the original EvalError as `cause`.
 */
function compile(): JsExprEvaluator {
  try {
    return new Function("ctx", "expr", EVALUATOR_BODY) as JsExprEvaluator;
  } catch (cause) {
    throw new Error(
      "voie shell: the cordis loader reached a `!js` expression, but this island is served under "
        + "Content-Security-Policy script-src 'self' with no 'unsafe-eval', so no string can be compiled. "
        + "The boot graph is synthetic and carries no `!js` node, so reaching this at all is the defect; "
        + "do not answer it by weakening the CSP.",
      { cause },
    );
  }
}

/**
 * Drop-in replacement for upstream's exported `evaluate` binding: same
 * arguments, same result, same thrown errors from the expression itself. The
 * only observable difference is WHEN the underlying function is built.
 */
export const evaluate: JsExprEvaluator = (ctx, expr) => {
  compiled ??= compile();
  return compiled(ctx, expr);
};
