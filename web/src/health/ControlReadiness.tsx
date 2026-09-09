import type {
  ControlReadinessDto,
  HealthActionDto,
} from "../api/health.ts";
import { Badge, Card } from "../ui/primitives.tsx";
import { actionKey, healthCardVariant, healthTone, observedText } from "./presentation.ts";

export type ControlReadinessProps = {
  control: ControlReadinessDto;
  retryAction: HealthActionDto | null;
  busyActionKey: string | null;
  onAction: (action: HealthActionDto) => void;
};

export function ControlReadiness({
  control,
  retryAction,
  busyActionKey,
  onAction,
}: ControlReadinessProps) {
  const retryKey = retryAction === null ? null : actionKey(retryAction);
  return (
    <Card
      title="Control readiness"
      variant={healthCardVariant(control.status)}
      actions={
        retryAction === null ? undefined : (
          <button
            type="button"
            className="btn"
            disabled={busyActionKey !== null}
            onClick={() => onAction(retryAction)}
          >
            {busyActionKey === retryKey ? "Retrying…" : retryAction.label}
          </button>
        )
      }
      bodyClass="kds-flush"
    >
      <div className="stack stack-tight table-toolbar">
        <p>
          <Badge tone={healthTone(control.status)}>{control.status}</Badge>
        </p>
        <p className="muted">
          Last observed: {observedText(control.lastObservedAt)}
        </p>
      </div>

      {control.checks.length === 0 ? (
        <p className="muted table-note">No control checks were reported by the server.</p>
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Check</th>
              <th scope="col">Status</th>
              <th scope="col">Detail</th>
              <th scope="col">HTTP</th>
              <th scope="col">Last observed</th>
            </tr>
          </thead>
          <tbody>
            {control.checks.map((check) => (
              <tr key={check.id}>
                <td>{check.label.trim().length === 0 ? "—" : check.label}</td>
                <td>
                  <Badge tone={healthTone(check.status)}>{check.status}</Badge>
                </td>
                <td>{check.detail === null || check.detail.trim().length === 0 ? "—" : check.detail}</td>
                <td className="mono">
                  {check.httpStatus === null ? "—" : check.httpStatus}
                </td>
                <td>{observedText(check.lastObservedAt)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {retryAction === null ? (
        <p className="muted table-note">
          No retry action was issued by the server for this admin session.
        </p>
      ) : null}
    </Card>
  );
}
