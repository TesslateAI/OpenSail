/* Queue-then-live module-loader facade (DSH rc.8 boot protocol). */
(() => {
  const pendingQueue = [];
  window.__ModuleLoader__ = {
    mode: "queue",
    pendingQueue,
    load(registration) {
      pendingQueue.push(registration);
    },
    create(options) {
      if (this.mode !== "queue") {
        throw new Error("client-modules: window.__ModuleLoader__.create called after module-system boot");
      }
      const id = "@deepseek-ai/dsh-client-modules";
      const index = pendingQueue.findIndex((registration) => registration.id === id);
      const registration = pendingQueue[index];
      if (registration === undefined) {
        throw new Error("client-modules: HTML did not preload @deepseek-ai/dsh-client-modules/client.js");
      }
      pendingQueue.splice(index, 1);
      const exports = registration.factory((specifier) => {
        throw new Error(
          'client-modules: @deepseek-ai/dsh-client-modules/client.js requested external "' +
            specifier +
            '" before the module system existed',
        );
      });
      if (
        typeof exports !== "object" ||
        exports === null ||
        typeof exports.createClientModuleSystem !== "function" ||
        typeof exports.apply !== "function"
      ) {
        throw new Error("client-modules: @deepseek-ai/dsh-client-modules/client.js did not export the bootstrap module face");
      }
      return exports.createClientModuleSystem(this, { id: registration.id, exports }, options);
    },
  };
})();
