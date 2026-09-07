/**
 * VOIE product shell around the conversation surface.
 *
 * The sidebar is the product IA: chats (New chat, Recent chats) sit above a
 * rule, then workspace operations (Workspaces, Team on team scopes, Secrets,
 * Settings). Legacy operator surfaces (Sessions, Agents, Project, Fabrics,
 * Audit) stay routable for deep links but sit out of regular-user navigation;
 * administration appears only from server-proven platform-admin facts
 * (`me.platformRole`, falling back to the verified directory probe).
 */

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { logout } from "../api/http.ts";
import { ProjectSwitcher } from "../projects/ProjectSwitcher.tsx";
import { CreateTeam } from "../team/CreateTeam.tsx";
import { useConsole } from "../console.tsx";
import { Link, appHref, useRouter, type Route } from "../router.tsx";
import { StateView } from "../ui/primitives.tsx";
import { CreateProjectForm } from "../ui/CreateProjectForm.tsx";
import { Login } from "../pages/Login.tsx";
import { PortalChatProvider, chatLabel } from "./chat-context.ts";
import { useRecentChats } from "./useRecentChats.ts";
import { ChatHome } from "./ChatHome.tsx";
import "./portal.css";

export type PortalShellProps = {
  /** Renders the route body inside the shell's content seat. */
  renderRoute: (route: Route) => ReactNode;
};

type NavLinkItem = {
  key: Route["name"];
  label: string;
  path: string;
};

const ADMIN_ITEMS: readonly NavLinkItem[] = [
  { key: "adminUsers", label: "Users", path: "/admin/users" },
  { key: "adminTeams", label: "Teams", path: "/admin/teams" },
  { key: "adminFabrics", label: "Fabrics", path: "/admin/fabric" },
  { key: "adminAuth", label: "Auth", path: "/admin/auth" },
  { key: "adminAudit", label: "System Audit", path: "/admin/audit" },
  { key: "adminHealth", label: "Health", path: "/admin/health" },
];

function NavGroup({
  label,
  items,
  activeName,
  projectId,
}: {
  label: string;
  items: readonly NavLinkItem[];
  activeName: Route["name"];
  projectId: string;
}) {
  return (
    <li>
      <span className="nav-group-label">{label}</span>
      <ul className="nav-list nav-list-nested">
        {items.map((item) => (
          <li key={item.key}>
            <Link
              className={activeName === item.key ? "nav-link nav-link-active" : "nav-link"}
              to={appHref(item.path, projectId)}
            >
              {item.label}
            </Link>
          </li>
        ))}
      </ul>
    </li>
  );
}

export function PortalShell({ renderRoute }: PortalShellProps) {
  const {
    me,
    projects,
    projectId,
    selectedProject,
    platformAdmin,
    loading,
    error,
    reload,
    setProjectId,
  } = useConsole();
  const { location } = useRouter();
  const [signingOut, setSigningOut] = useState(false);
  const recentChats = useRecentChats(projectId);
  const frozenChatId = useRef<string | undefined>(undefined);
  const prevRouteName = useRef(location.route.name);
  const [newChatGeneration, setNewChatGeneration] = useState(0);
  useEffect(() => {
    if (location.route.name === "home" && prevRouteName.current !== "home") {
      setNewChatGeneration((value) => value + 1);
    }
    prevRouteName.current = location.route.name;
  }, [location.route.name]);

  const handleSignOut = useCallback(() => {
    setSigningOut(true);
    void logout();
  }, []);

  // Login surface renders without requiring a loaded console; it is the 401
  // landing and the foundation redirects any expired session to it.
  if (location.route.name === "login") {
    return (
      <div className="boot">
        <Login />
      </div>
    );
  }

  // Every gate below must hold before the shell dereferences `me` or the
  // selected scope.
  if (error !== null) {
    return (
      <div className="boot">
        <StateView
          state="error"
          title="VOIE unavailable"
          detail={error.message}
          onRetry={reload}
        />
      </div>
    );
  }
  if (loading || me === null) {
    return (
      <div className="boot">
        <StateView
          state="loading"
          title="Preparing VOIE"
          detail="Loading your account and workspaces."
        />
      </div>
    );
  }
  if (projects.length === 0) {
    return (
      <div className="boot">
        <CreateProjectForm onCreated={reload} />
      </div>
    );
  }
  if (projectId === null) {
    return (
      <div className="boot">
        <StateView state="loading" title="Selecting project" />
      </div>
    );
  }

  // Human-first identity label: displayName, then username, and only a
  // short-ID fragment when the server provides neither. The full UUID stays
  // available in the tooltip for support triage.
  const displayName = me.displayName ?? "";
  const username = me.username ?? "";
  const accountLabel =
    displayName.trim() !== ""
      ? displayName.trim()
      : username.trim() !== ""
        ? username.trim()
        : me.userId.slice(0, 8);
  const accountTitle = `${displayName || username || me.userId} (${me.userId})`;

  const isChatRoute =
    location.route.name === "home" ||
    location.route.name === "chat" ||
    location.route.name === "session" ||
    location.route.name === "sessions";
  const workspacesActive =
    location.route.name === "workspaces" || location.route.name === "workspace";
  const applicationsActive =
    location.route.name === "applications" || location.route.name === "application";
  const teamActive = location.route.name === "team" || location.route.name === "projects";
  const chatConversationId =
    location.route.name === "chat"
      ? location.route.conversationId
      : location.route.name === "session"
        ? location.route.sessionId
        : undefined;
  if (isChatRoute) frozenChatId.current = chatConversationId;

  return (
    <PortalChatProvider
      value={{
        chats: recentChats.chats,
        loading: recentChats.loading,
        error: recentChats.error !== null,
        retry: recentChats.retry,
      }}
    >
      <div className={isChatRoute ? "shell portal-shell portal-shell--chat" : "shell portal-shell"}>
        <header className="topbar portal-topbar">
          <Link className="brand" to={appHref("/", projectId)}>
            <span className="brand-mark" aria-hidden="true" />
            <span className="brand-text">VOIE</span>
          </Link>
          <div className="topbar-spacer" />
          <div className="topbar-group">
            <ProjectSwitcher projects={projects} value={projectId} onChange={setProjectId} />
            <CreateTeam
              onCreated={(project) => {
                window.location.assign(`/team?project=${encodeURIComponent(project.id)}`);
              }}
            />
          </div>
          <div className="topbar-group">
            <span className="account" title={`Signed in as ${accountTitle}`}>
              <span className="mono account-subject">{accountLabel}</span>
            </span>
            <button type="button" className="btn" onClick={handleSignOut} disabled={signingOut}>
              {signingOut ? "Signing out…" : "Sign out"}
            </button>
          </div>
        </header>
        <div className="shell-body">
          <nav className="sidebar portal-sidebar" aria-label="Primary">
            <ul className="nav-list portal-nav-chats">
              <li>
                <Link
                  className={
                    isChatRoute ? "nav-link nav-link-active portal-new-chat" : "nav-link portal-new-chat"
                  }
                  to={appHref("/", projectId)}
                  onClick={() => {
                    if (location.route.name === "home") {
                      setNewChatGeneration((value) => value + 1);
                    }
                  }}
                >
                  <span aria-hidden="true">＋</span> New chat
                </Link>
              </li>
              <li>
                <span className="nav-group-label">Recent chats</span>
                <ul className="nav-list nav-list-nested portal-recents">
                  {recentChats.loading && recentChats.chats.length === 0 ? (
                    <li className="nav-empty">Loading…</li>
                  ) : recentChats.error ? (
                    <li className="nav-empty">
                      <button type="button" className="btn" onClick={recentChats.retry}>
                        Retry
                      </button>
                    </li>
                  ) : recentChats.chats.length === 0 ? (
                    <li className="nav-empty">No conversations yet</li>
                  ) : (
                    recentChats.chats.slice(0, 10).map((chat) => {
                      const active =
                        location.route.name === "chat" && location.route.conversationId === chat.id;
                      return (
                        <li key={chat.id}>
                          <Link
                            className={active ? "nav-link nav-link-active" : "nav-link"}
                            to={appHref(`/chat/${encodeURIComponent(chat.id)}`, projectId)}
                          >
                            {chatLabel(chat)}
                          </Link>
                        </li>
                      );
                    })
                  )}
                </ul>
              </li>
            </ul>
            <ul className="nav-list portal-nav-ops" aria-label="Workspace">
              <li>
                <Link
                  className={workspacesActive ? "nav-link nav-link-active" : "nav-link"}
                  to={appHref("/workspaces", projectId)}
                >
                  Workspaces
                </Link>
              </li>
              <li>
                <Link
                  className={applicationsActive ? "nav-link nav-link-active" : "nav-link"}
                  to={appHref("/applications", projectId)}
                >
                  Applications
                </Link>
              </li>
              {selectedProject?.kind === "team" ? (
                <li>
                  <Link className={teamActive ? "nav-link nav-link-active" : "nav-link"} to={appHref("/team", projectId)}>
                    Team
                  </Link>
                </li>
              ) : null}
              <li>
                <Link
                  className={
                    location.route.name === "secrets" ? "nav-link nav-link-active" : "nav-link"
                  }
                  to={appHref("/secrets", projectId)}
                >
                  Secrets
                </Link>
              </li>
              <li>
                <Link
                  className={
                    location.route.name === "settings" ? "nav-link nav-link-active" : "nav-link"
                  }
                  to={appHref("/settings", projectId)}
                >
                  Settings
                </Link>
              </li>
              {platformAdmin === true ? (
                <NavGroup label="Administration" items={ADMIN_ITEMS} activeName={location.route.name} projectId={projectId} />
              ) : null}
            </ul>
            <div className="sidebar-foot">
              <span>{selectedProject?.kind === "team" ? "Team" : "Personal"}</span>
              <span className="sidebar-project mono">{selectedProject?.name ?? projectId}</span>
            </div>
          </nav>
          <main className={isChatRoute ? "main main-chat" : "main"}>
            {/* The DSH module loader can boot only once unless the seat
                restores queue mode. Keep the chat graph mounted across
                management routes so New chat does not recreate it. */}
            <div className={isChatRoute ? "page page-chat" : "page page-inert"}>
              <ChatHome
                conversationId={isChatRoute ? chatConversationId : frozenChatId.current}
                newChatGeneration={newChatGeneration}
                seatActive={isChatRoute}
              />
            </div>
            {isChatRoute ? null : (
              <div className="page" key={location.pathname}>
                {renderRoute(location.route)}
              </div>
            )}
          </main>
        </div>
      </div>
    </PortalChatProvider>
  );
}
