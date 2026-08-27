/** Browser stand-in for `node:module`. Unreachable in the configured loader path. */
export const createRequire = (): never => {
  throw new Error("node:module is not available in the browser");
};

export type LoadHookContext = never;
