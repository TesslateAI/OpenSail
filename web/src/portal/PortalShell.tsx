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
import {
  IconChat,
  IconChevronDown,
  IconGauge,
  IconGrid,
  IconFolder,
  IconGear,
  IconLock,
  IconPanel,
  IconPlus,
  IconServer,
  IconShield,
  IconSignOut,
  IconTeam,
  IconUsers,
} from "../design-system/icons.tsx";
import { PortalChatProvider, chatLabel } from "./chat-context.ts";
import { useRecentChats } from "./useRecentChats.ts";
import { ChatHome } from "./ChatHome.tsx";
import "./portal.css";

export type PortalShellProps = {
  /** Renders the route body inside the shell's content seat. */
  renderRoute: (route: Route) => ReactNode;
};

type NavIcon = (props: { size?: number; className?: string }) => ReactNode;

type NavLinkItem = {
  key: Route["name"];
  label: string;
  path: string;
  icon: NavIcon;
};

const ADMIN_ITEMS: readonly NavLinkItem[] = [
  { key: "adminUsers", label: "Users", path: "/admin/users", icon: IconUsers },
  { key: "adminTeams", label: "Teams", path: "/admin/teams", icon: IconTeam },
  { key: "adminFabrics", label: "Fabrics", path: "/admin/fabric", icon: IconServer },
  { key: "adminAuth", label: "Auth", path: "/admin/auth", icon: IconLock },
  { key: "adminAudit", label: "System Audit", path: "/admin/audit", icon: IconShield },
  { key: "adminHealth", label: "Health", path: "/admin/health", icon: IconGauge },
];

/** Nav row glyph seat: 17px box, muted until the row goes active. */
function NavIconSeat({ icon: Glyph }: { icon: NavIcon }) {
  return (
    <span className="nav-icon">
      <Glyph size={17} />
    </span>
  );
}

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
    <li className="nav-section">
      <span className="nav-group-label">{label}</span>
      <ul className="nav-list nav-list-nested">
        {items.map((item) => (
          <li key={item.key}>
            <Link
              className={activeName === item.key ? "nav-link nav-link-active" : "nav-link"}
              to={appHref(item.path, projectId)}
              title={item.label}
            >
              <NavIconSeat icon={item.icon} />
              <span className="nav-text">{item.label}</span>
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
  const [collapsed, setCollapsed] = useState(false);
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

  // Sidebar collapse mirrors the mock: 280px rail down to a 48px icon rail,
  // never to zero. Nav rows keep their titles so the collapsed rail stays
  // legible to pointer and screen-reader users alike.
  const railLabel = collapsed ? "Expand sidebar" : "Collapse sidebar";
  const scopeKindLabel = selectedProject?.kind === "team" ? "Team" : "Personal";
  const scopeName = selectedProject?.name ?? projectId;
  const initials = accountLabel
    .split(/\s+/u)
    .filter((part) => part !== "")
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase() ?? "")
    .join("");

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
        <div className="shell-body">
          <nav
            className={
              collapsed ? "sidebar portal-sidebar collapsed" : "sidebar portal-sidebar"
            }
            aria-label="Primary"
          >
            <div className="sidebar-head">
              <Link className="brand" to={appHref("/", projectId)}>
                <span className="brand-mark" aria-hidden="true">
                  V
                </span>
                <span className="brand-text">
                  <span className="brand-name">VOIE</span>
                  <span className="brand-sub">{scopeKindLabel} workspace</span>
                </span>
              </Link>
            </div>
            <div className="sidebar-scroll">
              <ul className="nav-list portal-nav-chats">
                <li>
                  <Link
                    className={
                      isChatRoute
                        ? "nav-link nav-link-active portal-new-chat"
                        : "nav-link portal-new-chat"
                    }
                    to={appHref("/", projectId)}
                    title="New chat"
                    onClick={() => {
                      if (location.route.name === "home") {
                        setNewChatGeneration((value) => value + 1);
                      }
                    }}
                  >
                    <NavIconSeat icon={IconPlus} />
                    <span className="nav-text">New chat</span>
                  </Link>
                </li>
                <li className="nav-section">
                  <span className="nav-group-label">Recent chats</span>
                  <ul className="nav-list nav-list-nested portal-recents">
                    {recentChats.loading && recentChats.chats.length === 0 ? (
                      <li className="nav-empty">Loading…</li>
                    ) : recentChats.error ? (
                      <li className="nav-empty">
                        <button type="button" className="btn btn-sm" onClick={recentChats.retry}>
                          Retry
                        </button>
                      </li>
                    ) : recentChats.chats.length === 0 ? (
                      <li className="nav-empty">No conversations yet</li>
                    ) : (
                      recentChats.chats.slice(0, 10).map((chat) => {
                        const active =
                          location.route.name === "chat" &&
                          location.route.conversationId === chat.id;
                        const label = chatLabel(chat);
                        return (
                          <li key={chat.id}>
                            <Link
                              className={active ? "nav-link nav-link-active" : "nav-link"}
                              to={appHref(`/chat/${encodeURIComponent(chat.id)}`, projectId)}
                              title={label}
                            >
                              <NavIconSeat icon={IconChat} />
                              <span className="nav-text">{label}</span>
                            </Link>
                          </li>
                        );
                      })
                    )}
                  </ul>
                </li>
              </ul>
              <ul className="nav-list portal-nav-ops" aria-label="Workspace">
                <li className="nav-section">
                  <span className="nav-group-label">Workspace</span>
                  <ul className="nav-list nav-list-nested">
                    <li>
                      <Link
                        className={workspacesActive ? "nav-link nav-link-active" : "nav-link"}
                        to={appHref("/workspaces", projectId)}
                        title="Workspaces"
                      >
                        <NavIconSeat icon={IconFolder} />
                        <span className="nav-text">Workspaces</span>
                      </Link>
                    </li>
                    <li>
                      <Link
                        className={applicationsActive ? "nav-link nav-link-active" : "nav-link"}
                        to={appHref("/applications", projectId)}
                        title="Applications"
                      >
                        <NavIconSeat icon={IconGrid} />
                        <span className="nav-text">Applications</span>
                      </Link>
                    </li>
                    {selectedProject?.kind === "team" ? (
                      <li>
                        <Link
                          className={teamActive ? "nav-link nav-link-active" : "nav-link"}
                          to={appHref("/team", projectId)}
                          title="Team"
                        >
                          <NavIconSeat icon={IconTeam} />
                          <span className="nav-text">Team</span>
                        </Link>
                      </li>
                    ) : null}
                    <li>
                      <Link
                        className={
                          location.route.name === "secrets" ? "nav-link nav-link-active" : "nav-link"
                        }
                        to={appHref("/secrets", projectId)}
                        title="Secrets"
                      >
                        <NavIconSeat icon={IconLock} />
                        <span className="nav-text">Secrets</span>
                      </Link>
                    </li>
                    <li>
                      <Link
                        className={
                          location.route.name === "settings"
                            ? "nav-link nav-link-active"
                            : "nav-link"
                        }
                        to={appHref("/settings", projectId)}
                        title="Settings"
                      >
                        <NavIconSeat icon={IconGear} />
                        <span className="nav-text">Settings</span>
                      </Link>
                    </li>
                  </ul>
                </li>
                {platformAdmin === true ? (
                  <NavGroup
                    label="Administration"
                    items={ADMIN_ITEMS}
                    activeName={location.route.name}
                    projectId={projectId}
                  />
                ) : null}
              </ul>
            </div>
            <div className="sidebar-foot">
              <div className="profile portal-profile">
                <span className="avatar" aria-hidden="true">
                  {initials === "" ? "V" : initials}
                </span>
                <span className="profile-text account" title={`Signed in as ${accountTitle}`}>
                  <span className="profile-name account-subject">{accountLabel}</span>
                  <span className="profile-role sidebar-project">{scopeName}</span>
                </span>
                <button
                  type="button"
                  className="icon-btn portal-signout"
                  onClick={handleSignOut}
                  disabled={signingOut}
                  aria-label={signingOut ? "Signing out…" : "Sign out"}
                  title={signingOut ? "Signing out…" : "Sign out"}
                >
                  <IconSignOut size={17} />
                </button>
              </div>
            </div>
          </nav>
          <main className={isChatRoute ? "main main-chat" : "main"}>
            <header className="topbar portal-topbar">
              <button
                type="button"
                className="icon-btn"
                onClick={() => setCollapsed((value) => !value)}
                aria-label={railLabel}
                title={railLabel}
                aria-expanded={!collapsed}
              >
                <IconPanel size={16} />
              </button>
              <div className="crumbs">
                <span className="cur">{scopeKindLabel}</span>
                <span className="sep">/</span>
                <span className="cur">{scopeName}</span>
              </div>
              <div className="topbar-spacer" />
              <div className="topbar-group">
                <ProjectSwitcher projects={projects} value={projectId} onChange={setProjectId} />
                <CreateTeam
                  onCreated={(project) => {
                    window.location.assign(`/team?project=${encodeURIComponent(project.id)}`);
                  }}
                />
              </div>
            </header>
            {/* The DSH module loader can boot only once unless the seat
                restores queue mode. Keep the chat graph mounted across
                management routes so New chat does not recreate it. */}
            <div className={isChatRoute ? "content content-chat" : "content"}>
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
            </div>
          </main>
        </div>
      </div>
    </PortalChatProvider>
  );
}
