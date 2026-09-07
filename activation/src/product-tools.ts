import { Context, Service } from "@deepseek-ai/cordis";
import type { CallTracker, EventSource, ParentLink, ProductToolSpec } from "./parent.js";

// dsh-tools 0.1.0-rc.6 refuses register() without a canonical output
// declaration. Product tools return one parent-authored text payload.
const PRODUCT_OUTPUT = {
  schema: {
    type: "object",
    additionalProperties: false,
    required: ["text"],
    properties: {
      text: { type: "string" },
    },
  },
  render(_args: unknown, value: unknown) {
    const text =
      value !== null &&
      typeof value === "object" &&
      "text" in value &&
      typeof (value as { text: unknown }).text === "string"
        ? (value as { text: string }).text
        : "";
    return [{ type: "text" as const, text }];
  },
};

export interface ProductToolDeps {
  parent: ParentLink;
  events: EventSource;
  calls: CallTracker;
  tools: ReadonlyArray<ProductToolSpec>;
}

/**
 * Registers typed Application-platform tools that execute only on the parent.
 * The child never receives Fabric, Blob, or secret material. Definitions come
 * from the parent hello bootstrap; this module does not keep a second copy.
 */
export default class ParentProductTools extends Service {
  static readonly inject = ["tools"];

  constructor(ctx: Context, deps: ProductToolDeps) {
    super(ctx, "voie.product-tools");
    if (deps.tools.length === 0) {
      throw new Error("parent bootstrap lacked product tools");
    }
    const tools = (ctx as Context & { tools?: { register?(tool: unknown): void; define?(tool: unknown): void } }).tools;
    const register = tools?.register ?? tools?.define;
    if (typeof register !== "function") {
      throw new Error("DSH tool runtime is not installed");
    }
    for (const spec of deps.tools) {
      register.call(tools, {
        name: spec.name,
        description: spec.description,
        parameters: spec.parameters,
        output: PRODUCT_OUTPUT,
        async execute(args: Record<string, unknown>) {
          const call_id = deps.calls.take();
          if (call_id === undefined) {
            throw new Error("product intent has no outstanding model call id");
          }
          const reply = await deps.parent.product({
            call_id,
            name: spec.name,
            arguments: args,
            events: deps.events.collect(),
          });
          deps.events.advance();
          if (reply.is_error) {
            throw new Error(reply.text || `${spec.name} failed`);
          }
          return { text: reply.text };
        },
      });
    }
  }
}
