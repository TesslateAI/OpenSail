import type { DeploymentServiceDto } from "../api/health.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import {
  healthCardVariant,
  healthTone,
  observedText,
  summaryHealthStatus,
} from "./presentation.ts";

export type DeploymentServicesProps = {
  services: readonly DeploymentServiceDto[];
};

export function DeploymentServices({ services }: DeploymentServicesProps) {
  const status = summaryHealthStatus(services.map((service) => service.status));
  return (
    <Card
      title={`Deployment & services (${services.length})`}
      variant={healthCardVariant(status)}
      bodyClass="kds-flush"
    >
      {services.length === 0 ? (
        <StateView
          state="empty"
          title="No service observations"
          detail="The server returned no deployment or service probe rows."
        />
      ) : (
        <>
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Service</th>
                <th scope="col">Kind</th>
                <th scope="col">Status</th>
                <th scope="col">Detail</th>
                <th scope="col">HTTP</th>
                <th scope="col">Last observed</th>
              </tr>
            </thead>
            <tbody>
              {services.map((service) => (
                <tr key={service.id}>
                  <td>{service.name.trim().length === 0 ? "—" : service.name}</td>
                  <td className="mono">
                    {service.kind.trim().length === 0 ? "—" : service.kind}
                  </td>
                  <td>
                    <Badge tone={healthTone(service.status)}>{service.status}</Badge>
                  </td>
                  <td>
                    {service.detail === null || service.detail.trim().length === 0
                      ? "—"
                      : service.detail}
                  </td>
                  <td className="mono">
                    {service.httpStatus === null ? "—" : service.httpStatus}
                  </td>
                  <td className="kds-datetime">{observedText(service.lastObservedAt)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="muted">
            Readiness is fail-closed across the control database, configured dependencies, and built
            web assets. Values above are observations, not deployment commands.
          </p>
        </>
      )}
    </Card>
  );
}
