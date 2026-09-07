import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { getMe, listProjects } from "./api/api.ts";
import { directoryApi } from "./api/directory.ts";
import { ApiError } from "./api/http.ts";
import type { MeDto, ProjectSummaryDto, Role } from "./api/dto.ts";
import { useResource } from "./hooks.ts";
import { NAVIGATION_EVENT, projectSwitchPath } from "./router.tsx";

export type ConsoleContextValue = {
  me: MeDto | null;
  projects: ProjectSummaryDto[];
  projectId: string | null;
  selectedProject: ProjectSummaryDto | null;
  role: Role;
  canOperate: boolean;
  canManageMembers: boolean;
  loading: boolean;
  error: Error | null;
  reload: () => void;
  setProjectId: (projectId: string) => void;
  /** null while the platform-admin probe is unresolved; false hides admin nav. */
  platformAdmin: boolean | null;
};

type Bootstrap = {
  me: MeDto;
  projects: ProjectSummaryDto[];
};

const ConsoleContext = createContext<ConsoleContextValue | null>(null);

const PROJECT_STORAGE_KEY = "voie:projectId";

function projectFromLocation(): string | null {
  try {
    const params = new URLSearchParams(window.location.search);
    const value = params.get("project");
    return value !== null && value.trim() !== "" ? value.trim() : null;
  } catch {
    return null;
  }
}

function storedProjectId(): string | null {
  try {
    const value = window.localStorage.getItem(PROJECT_STORAGE_KEY);
    return value !== null && value.trim() !== "" ? value.trim() : null;
  } catch {
    return null;
  }
}

function persistProjectId(next: string | null): void {
  try {
    if (next === null) window.localStorage.removeItem(PROJECT_STORAGE_KEY);
    else window.localStorage.setItem(PROJECT_STORAGE_KEY, next);
  } catch {
    // Storage may be unavailable in some embed contexts.
  }
}

function syncProjectToUrl(next: string | null): void {
  try {
    const url = new URL(window.location.href);
    if (next === null) url.searchParams.delete("project");
    else url.searchParams.set("project", next);
    const nextSearch = url.search;
    const nextHash = url.hash;
    const nextPath = `${url.pathname}${nextSearch}${nextHash}`;
    const current = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    if (nextPath !== current) window.history.replaceState(null, "", nextPath);
  } catch {
    // Non-URL environments (tests) ignore syncing.
  }
}

export function ConsoleProvider({ children }: { children: ReactNode }) {
  const load = useCallback(async (signal: AbortSignal): Promise<Bootstrap> => {
    const [me, projects] = await Promise.all([
      getMe(signal),
      listProjects(signal),
    ]);
    return { me, projects };
  }, []);
  const resource = useResource(load);
  const [projectId, setProjectId] = useState<string | null>(null);
  const [platformAdmin, setPlatformAdmin] = useState<boolean | null>(null);

  // Platform-admin nav gates on the explicit /api/me `platformRole` when the
  // session shape reports it ("admin" reveals, "user" hides — no probe).
  // Only when the field is absent does the verified directory read decide;
  // refusal or transport failure keeps the group hidden, and the value stays
  // null only while that fallback probe is in flight.
  useEffect(() => {
    if (resource.data === null || resource.data.me.userId === "") return;
    const explicitRole = resource.data.me.platformRole;
    if (explicitRole === "admin" || explicitRole === "user") {
      setPlatformAdmin(explicitRole === "admin");
      return;
    }
    const controller = new AbortController();
    const probe = async (): Promise<void> => {
      try {
        await directoryApi.listAdminUsers(controller.signal);
        setPlatformAdmin(true);
      } catch (error) {
        if (controller.signal.aborted) return;
        // Any refusal or transport failure hides the admin group; the probe
        // only ever promotes a verified read to `true`.
        if (error instanceof ApiError && error.status === 403) {
          setPlatformAdmin(false);
        } else {
          setPlatformAdmin(false);
        }
      }
    };
    void probe();
    return () => controller.abort();
  }, [resource.data]);

  useEffect(() => {
    const bootstrap = resource.data;
    if (bootstrap === null || bootstrap.projects.length === 0) {
      setProjectId(null);
      return;
    }
    const candidateFromUrl = projectFromLocation();
    const candidateFromStorage = storedProjectId();
    setProjectId((current) => {
      if (current !== null && bootstrap.projects.some((project) => project.id === current)) {
        return current;
      }
      if (
        candidateFromUrl !== null &&
        bootstrap.projects.some((project) => project.id === candidateFromUrl)
      ) {
        return candidateFromUrl;
      }
      if (
        candidateFromStorage !== null &&
        bootstrap.projects.some((project) => project.id === candidateFromStorage)
      ) {
        return candidateFromStorage;
      }
      const personal = bootstrap.projects.find((project) => project.kind === "personal");
      return personal?.id ?? bootstrap.projects[0]?.id ?? null;
    });
  }, [resource.data]);

  useEffect(() => {
    // projectId is null only while bootstrap has not chosen a project.
    // Persisting that null would erase a stored/URL selection on every
    // reload before the bootstrap effect can read it.
    if (projectId === null) return;
    persistProjectId(projectId);
    const sync = (): void => {
      syncProjectToUrl(projectId);
    };
    sync();
    // Internal navigations often omit `?project=`. Re-stamp it after every
    // path change so a copied `/chat/:id` URL still names the owning scope.
    window.addEventListener("popstate", sync);
    window.addEventListener(NAVIGATION_EVENT, sync);
    return () => {
      window.removeEventListener("popstate", sync);
      window.removeEventListener(NAVIGATION_EVENT, sync);
    };
  }, [projectId]);

  const setProjectIdAndSync = useCallback((next: string) => {
    setProjectId((current) => {
      if (current === next) return current;
      persistProjectId(next);
      // DSH's module loader can boot only once per document. The carrier
      // binds project at graph boot, so a user project change needs a fresh
      // page rather than a second in-page mount.
      const url = projectSwitchPath(window.location.pathname, next);
      window.setTimeout(() => {
        window.location.assign(url);
      }, 0);
      return next;
    });
  }, []);

  const selectedProject = useMemo(
    () => resource.data?.projects.find((project) => project.id === projectId) ?? null,
    [projectId, resource.data],
  );
  // Permissions come straight from the server-emitted capability set. The
  // role label stays display-only; it never gates an action here.
  const role: Role = selectedProject?.role ?? "viewer";
  const canOperate = selectedProject?.capabilities.operateSessions === true;
  const canManageMembers = selectedProject?.capabilities.manageMembers === true;
  const value = useMemo<ConsoleContextValue>(() => ({
    me: resource.data?.me ?? null,
    projects: resource.data?.projects ?? [],
    projectId,
    selectedProject,
    role,
    canOperate,
    canManageMembers,
    loading: resource.loading,
    error: resource.error,
    reload: resource.reload,
    setProjectId: setProjectIdAndSync,
    platformAdmin,
  }), [
    canManageMembers,
    canOperate,
    platformAdmin,
    projectId,
    resource.data,
    resource.error,
    resource.loading,
    resource.reload,
    role,
    selectedProject,
    setProjectIdAndSync,
  ]);

  return <ConsoleContext.Provider value={value}>{children}</ConsoleContext.Provider>;
}

export function useConsole(): ConsoleContextValue {
  const context = useContext(ConsoleContext);
  if (context === null) throw new Error("useConsole must be used inside ConsoleProvider");
  return context;
}
