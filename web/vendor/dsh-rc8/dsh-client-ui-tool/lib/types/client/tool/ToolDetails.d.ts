import type { ToolDetailsProps } from '../contract/slots.ts';
/**
 * Render the selected Tool call's structured output when its presentation
 * intent is known, otherwise preserve the flattened result text.
 * @param props - selected call slice, workspace root, host home, and locale seat.
 * @returns the details output body.
 */
export declare function ToolDetails({ block, cwd, useHostDescription, t, }: Pick<ToolDetailsProps, 'block' | 'cwd' | 'useHostDescription' | 't'>): import("react").JSX.Element;
//# sourceMappingURL=ToolDetails.d.ts.map