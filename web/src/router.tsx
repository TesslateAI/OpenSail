import { useCallback, useEffect, useState, type MouseEvent, type ReactNode } from "react";
import type { Uuid } from "./api/dto.ts";

export type Route =
  | { name: "home" }
  | { name: "chat"; conversationId: Uuid }
  | { name: "workspaces" }
  | { name: "workspace"; workspaceId: Uuid }
  | { name: "team" }
  | { name: "scopes"; scopeId: Uuid | null }
  | { name: "secrets" }
  | { name: "settings" }
  | { name: "agents" }
  | { name: "sessions" }
  | { name: "session"; sessionId: Uuid }
  | { name: "project" }
  | { name: "adminUsers" }
  | { name: "adminScopesTeams" }
  | { name: "adminFabrics" }
  | { name: "adminAuth" }
  | { name: "adminAudit" }
  | { name: "adminHealth" }
  | { name: "login" };

export type LocationState = {
  pathname: string;
  search: string;
  route: Route;
};

function decodeSegment(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

export function parseLocation(pathname: string): Route {
  const segments = pathname.replace(/\/+$/, "").split("/").filter(Boolean);
  const section = segments[0];
  // Session detail keeps its route name: the legacy pages (Sessions list,
  // Session view) read `route.name === "session"` for their active state.
  if (section === "sessions" && segments[1] !== undefined) {
    return { name: "session", sessionId: decodeSegment(segments[1]) };
  }
  if (section === "chat" && segments[1] !== undefined) {
    return { name: "chat", conversationId: decodeSegment(segments[1]) };
  }
  if (section === "workspace" && segments[1] !== undefined) {
    return { name: "workspace", workspaceId: decodeSegment(segments[1]) };
  }
  if (section === "scopes") {
    return { name: "scopes", scopeId: segments[1] !== undefined ? decodeSegment(segments[1]) : null };
  }
  switch (section) {
    case "workspaces":
      return { name: "workspaces" };
    case "team":
      return { name: "team" };
    case "secrets":
      return { name: "secrets" };
    case "settings":
      return { name: "settings" };
    case "agents":
      return { name: "agents" };
    case "sessions":
      return { name: "sessions" };
    case "project":
      return { name: "project" };
    case "admin":
      switch (segments[1]) {
        case "users":
          return { name: "adminUsers" };
        case "scopes":
          return { name: "adminScopesTeams" };
        case "fabric":
        case "fabrics":
          return { name: "adminFabrics" };
        case "auth":
          return { name: "adminAuth" };
        case "audit":
          return { name: "adminAudit" };
        case "health":
          return { name: "adminHealth" };
        default:
          return { name: "adminAudit" };
      }
    case "login":
      return { name: "login" };
    default:
      // Everything else (including the retired operator sections and bare
      // paths) lands on the conversation-first home surface.
      return { name: "home" };
  }
}

function readLocation(): LocationState {
  return {
    pathname: window.location.pathname,
    search: window.location.search,
    route: parseLocation(window.location.pathname),
  };
}

export function projectFromSearch(search: string): Uuid | null {
  try {
    const params = new URLSearchParams(search);
    const value = params.get("project");
    return value !== null && value.trim() !== "" ? value.trim() : null;
  } catch {
    return null;
  }
}

export function withProjectSearch(path: string, projectId: Uuid | null, currentSearch: string): string {
  if (projectId === null) return path;
  const separator = path.includes("?") ? "&" : "?";
  if (path.includes("project=")) return path;
  void currentSearch;
  return `${path}${separator}project=${encodeURIComponent(projectId)}`;
}

const NAVIGATION_EVENT = "voie:navigate";

function emitNavigation(): void {
  window.dispatchEvent(new Event(NAVIGATION_EVENT));
}

export function useRouter(): {
  location: LocationState;
  navigate: (to: string, replace?: boolean) => void;
} {
  const [location, setLocation] = useState<LocationState>(readLocation);
  useEffect(() => {
    const update = (): void => setLocation(readLocation());
    window.addEventListener("popstate", update);
    window.addEventListener(NAVIGATION_EVENT, update);
    return () => {
      window.removeEventListener("popstate", update);
      window.removeEventListener(NAVIGATION_EVENT, update);
    };
  }, []);
  const navigate = useCallback((to: string, replace = false): void => {
    if (replace) window.history.replaceState(null, "", to);
    else window.history.pushState(null, "", to);
    emitNavigation();
  }, []);
  return { location, navigate };
}

export function routeToPath(route: Route): string {
  switch (route.name) {
    case "chat":
      return `/chat/${encodeURIComponent(route.conversationId)}`;
    case "workspaces":
      return "/workspaces";
    case "workspace":
      return `/workspace/${encodeURIComponent(route.workspaceId)}`;
    case "team":
      return "/team";
    case "scopes":
      return route.scopeId === null ? "/scopes" : `/scopes/${encodeURIComponent(route.scopeId)}`;
    case "secrets":
      return "/secrets";
    case "settings":
      return "/settings";
    case "agents":
      return "/agents";
    case "sessions":
      return "/sessions";
    case "session":
      return `/sessions/${encodeURIComponent(route.sessionId)}`;
    case "project":
      return "/project";
    case "adminUsers":
      return "/admin/users";
    case "adminScopesTeams":
      return "/admin/scopes";
    case "adminFabrics":
      return "/admin/fabric";
    case "adminAuth":
      return "/admin/auth";
    case "adminAudit":
      return "/admin/audit";
    case "adminHealth":
      return "/admin/health";
    case "login":
      return "/login";
    case "home":
      return "/";
  }
}

export function appHref(path: string, projectId: Uuid | null): string {
  if (projectId === null) return path;
  const separator = path.includes("?") ? "&" : "?";
  if (path.includes("project=")) return path;
  return `${path}${separator}project=${encodeURIComponent(projectId)}`;
}

export type LinkProps = {
  to: string;
  children: ReactNode;
  className?: string;
  replace?: boolean;
  onClick?: () => void;
};

export function Link({ to, children, className, replace = false, onClick }: LinkProps) {
  const { navigate } = useRouter();
  const handleClick = (event: MouseEvent<HTMLAnchorElement>): void => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    onClick?.();
    navigate(to, replace);
  };
  return <a className={className} href={to} onClick={handleClick}>{children}</a>;
}
