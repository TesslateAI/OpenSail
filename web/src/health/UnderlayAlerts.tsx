import type { UnderlayAlertDto } from "../api/health.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { alertTone, observedText } from "./presentation.ts";

export type UnderlayAlertsProps = {
  alerts: readonly UnderlayAlertDto[];
};

function cardVariant(alerts: readonly UnderlayAlertDto[]) {
  if (alerts.some((alert) => alert.severity === "critical")) return "failure" as const;
  if (alerts.some((alert) => alert.severity === "warning")) return "default" as const;
  return "unknown" as const;
}

export function UnderlayAlerts({ alerts }: UnderlayAlertsProps) {
  return (
    <Card title={`Underlay alerts (${alerts.length})`} variant={cardVariant(alerts)}>
      {alerts.length === 0 ? (
        <StateView
          state="empty"
          title="No underlay alerts"
          detail="No non-ok audit outcomes were returned in the latest verified audit window."
        />
      ) : (
        <>
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Severity</th>
                <th scope="col">Source</th>
                <th scope="col">Alert</th>
                <th scope="col">Occurred</th>
                <th scope="col">Last observed</th>
              </tr>
            </thead>
            <tbody>
              {alerts.map((alert) => (
                <tr key={alert.id}>
                  <td>
                    <Badge tone={alertTone(alert.severity)}>{alert.severity}</Badge>
                  </td>
                  <td className="mono">
                    {alert.source.trim().length === 0 ? "—" : alert.source}
                  </td>
                  <td>
                    <div>{alert.message.trim().length === 0 ? "—" : alert.message}</div>
                    {alert.detail === null || alert.detail.trim().length === 0 ? null : (
                      <div className="muted">{alert.detail}</div>
                    )}
                  </td>
                  <td>{observedText(alert.occurredAt)}</td>
                  <td>{observedText(alert.lastObservedAt)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="muted">
            Alerts expose audit kind, outcome, and timestamps only. Payloads, project identifiers,
            Session identifiers, and Fabric internals are not rendered here.
          </p>
        </>
      )}
    </Card>
  );
}
