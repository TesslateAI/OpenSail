import type { ClientContext } from '@deepseek-ai/dsh-client-runtime/client';
/** Required services: the slot registry and the Host description used for POSIX `~`. */
export declare const inject: string[];
/**
 * Mount the whole-Tool renderers and built-in atomic Tool registrations.
 * @param ctx - Client root context.
 */
export declare function apply(ctx: ClientContext): void;
//# sourceMappingURL=apply.d.ts.map