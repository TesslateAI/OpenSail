/**
 * Auth: platform-admin view of the server-declared authentication surface.
 *
 * The page renders exactly what `GET /api/auth/capabilities` publishes — the
 * native credential switch and every external identity-provider action — and
 * never guesses which login routes exist. It stays read-only: account and
 * session administration live on the Users surface where their mutations
 * belong.
 */

import { useCallback } from "react";
import { getAuthCapabilities } from "../api/auth.ts";
import type { AuthCapabilitiesDto } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function AdminAuthCapabilities({ capabilities }: { capabilities: AuthCapabilitiesDto }) {
  return (
    <Card title="Sign-in surfaces">
      <p>
        Native credentials:{" "}
        <Badge tone={capabilities.native ? "accent" : "neutral"}>
          {capabilities.native ? "enabled" : "disabled"}
        </Badge>
      </p>
      {capabilities.external.length === 0 ? (
        <p>No external identity providers are configured.</p>
      ) : (
        <ul>
          {capabilities.external.map((provider) => (
            <li key={provider.id}>
              {provider.label} <span className="mono">{provider.id}</span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

export function AdminAuth() {
  const load = useCallback(
    (signal: AbortSignal): Promise<AuthCapabilitiesDto> => getAuthCapabilities(signal),
    [],
  );
  const resource = useResource(load);

  if (resource.error !== null) {
    return (
      <section className="portal-panel">
        <PageHeader
          title="Auth"
          subtitle="Platform sign-in surfaces as the control plane declares them."
        />
        <StateView
          state="error"
          title="Auth capabilities unavailable"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </section>
    );
  }
  if (resource.data === null) {
    return (
      <section className="portal-panel">
        <PageHeader
          title="Auth"
          subtitle="Platform sign-in surfaces as the control plane declares them."
        />
        <StateView state="loading" title="Reading auth capabilities" />
      </section>
    );
  }

  return (
    <section className="portal-panel">
      <PageHeader
        title="Auth"
        subtitle="Platform sign-in surfaces as the control plane declares them."
      />
      <AdminAuthCapabilities capabilities={resource.data} />
    </section>
  );
}
