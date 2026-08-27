import type { RefObject } from 'react';
/**
 * Close an open popover when a pointerdown lands outside its root element.
 * @param root - element containing both the trigger and the open surface.
 * @param open - whether the surface is showing; false detaches the listener.
 * @param setOpen - state setter invoked with false on an outside pointerdown.
 */
export declare function useDismissOnOutsidePointer(root: RefObject<HTMLElement | null>, open: boolean, setOpen: (open: boolean) => void): void;
//# sourceMappingURL=useDismissOnOutsidePointer.d.ts.map