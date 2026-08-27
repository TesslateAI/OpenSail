/**
 * Platform-admin health surface.
 *
 * The shell can mount `AdminHealth` when its server-owned platform-admin
 * entry is present. Until then these components remain unmounted and cannot
 * expose Fabric or underlay data through the regular-user navigation.
 */

export { AdminHealth } from "./AdminHealth.tsx";
export type { AdminHealthProps } from "./AdminHealth.tsx";
export { ControlReadiness } from "./ControlReadiness.tsx";
export type { ControlReadinessProps } from "./ControlReadiness.tsx";
export { DeploymentServices } from "./DeploymentServices.tsx";
export type { DeploymentServicesProps } from "./DeploymentServices.tsx";
export { FabricsCapacity } from "./FabricsCapacity.tsx";
export type { FabricsCapacityProps } from "./FabricsCapacity.tsx";
export { UnderlayAlerts } from "./UnderlayAlerts.tsx";
export type { UnderlayAlertsProps } from "./UnderlayAlerts.tsx";
