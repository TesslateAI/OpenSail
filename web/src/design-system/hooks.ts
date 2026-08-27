/**
 * Optional effect hooks for the parity system.
 *
 * The design-system components are intentionally effectless (controlled);
 * these hooks exist so consumers get the modal/drawer behaviours the mock
 * shipped (Escape closes), without forcing every screen to reimplement them.
 * Source: mock review-ui.js § NotificationBell / ui.js § Modal (Escape wiring).
 */
import { useEffect } from "react";

/** Calls `onDismiss` when Escape is pressed while `active`. */
export function useEscapeToClose(active: boolean, onDismiss: () => void): void {
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") onDismiss();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, onDismiss]);
}
