/** VOIE portal application shell and route-to-surface mapping. */

import { ConsoleProvider } from "./console.tsx";
import { type Route } from "./router.tsx";
import { PortalShell } from "./portal/PortalShell.tsx";
import { ChatHome } from "./portal/ChatHome.tsx";
import {
  ScopesPage,
  SecretVaultPage,
  TeamPage,
  WorkspaceDetailsPage,
  WorkspacesPage,
} from "./portal/PortalPanels.tsx";
import { UserSettingsPanel } from "./portal/UserSettingsPanel.tsx";
import { Applications } from "./applications/Applications.tsx";
import { ApplicationDetails } from "./applications/ApplicationDetails.tsx";
import { Agents } from "./pages/Agents.tsx";
import { Login } from "./pages/Login.tsx";
import { Project } from "./pages/Project.tsx";
import { Session } from "./pages/Session.tsx";
import { Sessions } from "./pages/Sessions.tsx";
import { AdminAuth, AdminUsersPage } from "./admin/index.ts";
import { AdminScopesTeams } from "./admin/ScopesTeams.tsx";
import { AdminFabricsUnderlay } from "./admin/FabricsUnderlay.tsx";
import { AdminSystemAudit } from "./admin/SystemAudit.tsx";
import { AdminHealth } from "./health/AdminHealth.tsx";

function RoutePage({ route }: { route: Route }) {
  switch (route.name) {
    case "home":
      return <ChatHome />;
    case "chat":
      return <ChatHome conversationId={route.conversationId} />;
    case "workspaces":
      return <WorkspacesPage />;
    case "workspace":
      return <WorkspaceDetailsPage workspaceId={route.workspaceId} />;
    case "applications":
      return <Applications />;
    case "application":
      return <ApplicationDetails applicationId={route.applicationId} />;
    case "team":
      return <TeamPage />;
    case "scopes":
      return <ScopesPage scopeId={route.scopeId} />;
    case "secrets":
      return <SecretVaultPage />;
    case "settings":
      return <UserSettingsPanel />;
    // Compatibility/deep-link routes. They are intentionally absent from
    // ordinary user navigation; the product path is conversation-first.
    case "session":
      return <Session />;
    case "sessions":
      return <Sessions />;
    case "agents":
      return <Agents />;
    case "project":
      return <Project />;
    case "adminUsers":
      return <AdminUsersPage />;
    case "adminScopesTeams":
      return <AdminScopesTeams />;
    case "adminFabrics":
      return <AdminFabricsUnderlay />;
    case "adminAuth":
      return <AdminAuth />;
    case "adminAudit":
      return <AdminSystemAudit />;
    case "adminHealth":
      return <AdminHealth />;
    case "login":
      return <Login />;
  }
}

export function App() {
  return (
    <ConsoleProvider>
      <PortalShell renderRoute={(route) => <RoutePage route={route} />} />
    </ConsoleProvider>
  );
}
