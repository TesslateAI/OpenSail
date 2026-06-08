"""
MCP marketplace install/manage CRUD endpoints.

All endpoints require JWT authentication. Handles installing MCP servers
from the marketplace, managing credentials, testing connections, and
discovering server capabilities.
"""

import logging
from typing import Any
from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel, Field
from sqlalchemy import func, select
from sqlalchemy.ext.asyncio import AsyncSession

from ..config import get_settings
from ..database import get_db
from ..models import (
    AgentMcpAssignment,
    MarketplaceAgent,
    MarketplaceSource,
    McpOAuthConnection,
    UserMcpConfig,
)
from ..schemas import (
    AgentMcpAssignmentResponse,
    McpConfigResponse,
    McpConfigUpdate,
    McpDiscoverResponse,
    McpInstallRequest,
    McpTestResponse,
)
from ..services.channels.registry import decrypt_credentials, encrypt_credentials
from ..services.marketplace_federation import install_guard, mcp_install_prompt
from ..services.mcp.client import connect_mcp
from ..services.mcp.oauth_redirect import build_mcp_oauth_callback_url
from ..users import current_active_user

logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/mcp", tags=["mcp"])
settings = get_settings()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _build_config_response(
    config: UserMcpConfig,
    agent: MarketplaceAgent | None = None,
    *,
    assigned_agent_ids: list[UUID] | None = None,
) -> McpConfigResponse:
    """Build a McpConfigResponse from a UserMcpConfig row.

    ``assigned_agent_ids`` is optional — bulk callers (the list endpoint)
    pre-fetch the assignments in one query and pass them in here; one-off
    callers that don't care about assignment state pass nothing.
    """
    agent_config = (agent.config or {}) if agent else {}
    is_oauth = agent_config.get("auth_type") == "oauth"

    # is_connected: real credential availability, not the row's enabled flag.
    # OAuth → tokens persisted on the paired McpOAuthConnection.
    # Static  → encrypted credentials present on the row.
    if is_oauth:
        oauth_row = getattr(config, "oauth_connection", None)
        is_connected = bool(oauth_row and getattr(oauth_row, "tokens_encrypted", ""))
    else:
        is_connected = bool(config.credentials)

    return McpConfigResponse(
        id=config.id,
        marketplace_agent_id=config.marketplace_agent_id,
        server_name=agent.name if agent else None,
        server_slug=agent.slug if agent else None,
        enabled_capabilities=config.enabled_capabilities,
        is_active=config.is_active,
        env_vars=agent_config.get("env_vars"),
        scope_level=config.scope_level,
        project_id=config.project_id,
        is_oauth=is_oauth,
        is_connected=is_connected,
        needs_reauth=bool(getattr(config, "needs_reauth", False)),
        last_auth_error=getattr(config, "last_auth_error", None),
        disabled_tools=config.disabled_tools,
        assigned_agent_ids=assigned_agent_ids or [],
        icon=agent.icon if agent else None,
        icon_url=agent.avatar_url if agent else None,
        created_at=config.created_at,
        updated_at=config.updated_at,
    )


async def _get_owned_config(
    config_id: UUID,
    user_id: UUID,
    db: AsyncSession,
    *,
    team_id: UUID | None = None,
) -> UserMcpConfig:
    """Fetch a UserMcpConfig and verify ownership. Raises 404 if missing.

    Ownership matches when the row belongs to ``user_id`` OR is on ``team_id``
    (when supplied) — either condition authorizes the caller. The previous
    ``if team_id else user_id`` form hid personal rows whenever a user had a
    default team, which broke every per-install endpoint for them.

    The ``oauth_connection`` relationship is eager-loaded so callers can
    read ``config.oauth_connection`` without tripping async lazy-load
    (``MissingGreenlet``) from a detached instance.
    """
    from sqlalchemy import or_ as _or
    from sqlalchemy.orm import selectinload as _selectin

    if team_id is not None:
        ownership_filter = _or(
            UserMcpConfig.user_id == user_id,
            UserMcpConfig.team_id == team_id,
        )
    else:
        ownership_filter = UserMcpConfig.user_id == user_id

    result = await db.execute(
        select(UserMcpConfig)
        .options(_selectin(UserMcpConfig.oauth_connection))
        .where(
            UserMcpConfig.id == config_id,
            ownership_filter,
            UserMcpConfig.is_active.is_(True),
        )
    )
    config = result.scalar_one_or_none()
    if not config:
        raise HTTPException(status_code=404, detail="MCP configuration not found")
    return config


async def _get_agent_for_config(
    marketplace_agent_id: UUID,
    db: AsyncSession,
) -> MarketplaceAgent:
    result = await db.execute(
        select(MarketplaceAgent).where(MarketplaceAgent.id == marketplace_agent_id)
    )
    agent = result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Marketplace agent not found")
    return agent


async def _invalidate_mcp_cache(user_id: UUID, user_mcp_config_id: UUID) -> None:
    """Invalidate Redis cache for a user/config pair (best-effort).

    Matches :class:`McpManager`'s cache key: ``mcp:schema:{user_id}:{config_id}``.
    """
    try:
        from ..services.cache_service import get_redis_client

        redis = await get_redis_client()
        if redis:
            cache_key = f"mcp:schema:{user_id}:{user_mcp_config_id}"
            await redis.delete(cache_key)
            logger.debug("Invalidated MCP cache: %s", cache_key)
    except Exception:
        logger.debug("MCP cache invalidation skipped (cache unavailable)")


async def _discover_server(
    agent: MarketplaceAgent,
    credentials: dict,
    *,
    user_mcp_config_id: UUID | None = None,
    db: AsyncSession | None = None,
) -> McpDiscoverResponse:
    """Connect to an MCP server and discover capabilities.

    OAuth connectors require the install row id and an active DB session so
    :func:`connect_mcp` can locate the stored OAuth tokens via
    :class:`PostgresTokenStorage`. Static-auth connectors ignore both args.
    """
    server_config = agent.config or {}
    async with connect_mcp(
        server_config,
        credentials,
        user_mcp_config_id=user_mcp_config_id,
        db=db,
    ) as session:
        # Each capability is optional per the MCP spec — Linear, for example,
        # exposes tools but not resources or prompts. Wrap each call so a
        # "method not supported" or schema-mismatch error on one capability
        # doesn't poison the whole discover (which used to surface as
        # "unhandled errors in a TaskGroup" → 502).
        async def _safe_list(label: str, coro):
            try:
                return await coro
            except Exception as exc:
                logger.debug("MCP %s on %s skipped: %s", label, agent.slug, exc)
                return None

        tools_result = await _safe_list("list_tools", session.list_tools())
        resources_result = await _safe_list("list_resources", session.list_resources())
        prompts_result = await _safe_list("list_prompts", session.list_prompts())
        resource_templates_result = await _safe_list(
            "list_resource_templates", session.list_resource_templates()
        )

        tools = [
            {"name": t.name, "description": t.description, "inputSchema": t.inputSchema}
            for t in (getattr(tools_result, "tools", None) or [])
        ]
        resources = [
            {"uri": str(r.uri), "name": r.name, "description": r.description}
            for r in (getattr(resources_result, "resources", None) or [])
        ]
        prompts = [
            {"name": p.name, "description": p.description}
            for p in (getattr(prompts_result, "prompts", None) or [])
        ]
        resource_templates = [
            {"uriTemplate": str(rt.uriTemplate), "name": rt.name, "description": rt.description}
            for rt in (getattr(resource_templates_result, "resourceTemplates", None) or [])
        ]

        return McpDiscoverResponse(
            tools=tools,
            resources=resources,
            prompts=prompts,
            resource_templates=resource_templates,
        )


# ---------------------------------------------------------------------------
# Endpoints
# ---------------------------------------------------------------------------


@router.post("/install", response_model=McpConfigResponse, status_code=201)
async def install_mcp_server(
    body: McpInstallRequest,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Install an MCP (Connector) server from the marketplace.

    Scope semantics (issue #307):
    - Default scope is ``user`` — the install lands on the caller with
      ``team_id=NULL`` and follows them across every team they belong to.
    - ``scope_level="project"`` is a per-user override pinned to a project
      and requires ``PROJECT_EDIT`` on the target project.
    - ``scope_level="team"`` is rejected with 400 because OAuth identities
      can't be shared across members (each user has their own tokens).

    The write is delegated to :func:`services.mcp.oauth_flow._upsert_user_mcp_config`
    so the static-credential and OAuth install paths converge on the same
    uniqueness tuple ``(user, agent, scope_level, team_id, project_id)``.
    """
    from ..permissions import Permission, get_project_with_access
    from ..services.mcp.oauth_flow import FlowState, _upsert_user_mcp_config

    # 1. Verify the marketplace item exists and is an MCP server.
    agent = await _get_agent_for_config(body.marketplace_agent_id, db)
    if agent.item_type != "mcp_server":
        raise HTTPException(
            status_code=400,
            detail="Marketplace item is not an MCP server",
        )

    # 1b. Wave 4: trust-level enforcement for MCP installs.
    #
    # Per the federated-marketplace plan, ``mcp_server`` installs from
    # ``untrusted`` sources are blocked outright; ``private`` sources
    # require explicit per-install confirmation that surfaces the
    # declared scope/tool list (and any destructive tools) to the user
    # before the install completes. The orchestrator never trusts the
    # client to enforce this — install_guard is the authoritative check.
    if agent.source_id is not None:
        source_row = (
            await db.execute(
                select(MarketplaceSource).where(MarketplaceSource.id == agent.source_id)
            )
        ).scalar_one_or_none()

        # The MCP install confirmation prompt is built from the agent's
        # config dict (which is the canonical place we keep the parsed
        # manifest fields the marketplace synced down). For apps installed
        # from a federated hub this gets enriched with tool/scope data;
        # for legacy installs from Tesslate Official it's typically the
        # OAuth client config which install_guard tolerates as empty
        # tools/scopes (the prompt simply renders an empty list).
        agent_config = agent.config if isinstance(agent.config, dict) else {}
        manifest: dict[str, Any] = {"manifest": agent_config}
        decision = install_guard(
            source_row,
            "mcp_server",
            version_meta=manifest,
            requester_user_id=user.id,
        )
        if not decision.allowed:
            raise HTTPException(
                status_code=403,
                detail={
                    "error": "install_blocked",
                    "reason": decision.reason,
                    "source_handle": source_row.handle if source_row else None,
                    "kind": "mcp_server",
                },
            )
        if decision.requires_confirmation and not body.confirmed:
            # Re-derive the structured prompt the UI renders. install_guard
            # already extracts the scope/tool list, but ``mcp_install_prompt``
            # also returns transport/command/url/env_keys — useful in the
            # confirmation modal so the user sees exactly what's being run.
            prompt = mcp_install_prompt(agent_config)
            raise HTTPException(
                status_code=409,
                detail={
                    "error": "install_requires_confirmation",
                    "reason": decision.reason,
                    "source_handle": source_row.handle if source_row else None,
                    "kind": "mcp_server",
                    "scope_tool_list": decision.scope_tool_list or [],
                    "destructive_tools": decision.destructive_tools,
                    "prompt": {
                        "transport": prompt.transport,
                        "command": prompt.command,
                        "url": prompt.url,
                        "args": prompt.args,
                        "env_keys": prompt.env_keys,
                        "scope_list": prompt.scope_list,
                    },
                },
            )

    # 2. Scope validation + RBAC.
    if body.scope_level not in ("user", "project"):
        raise HTTPException(
            status_code=400,
            detail=(
                "Unsupported scope_level. Connectors install at 'user' (default) or "
                "'project' scope. Team-scope install is not supported because OAuth "
                "identities cannot be shared across team members."
            ),
        )
    project_id = None
    if body.scope_level == "project":
        if not body.project_id:
            raise HTTPException(
                status_code=400,
                detail="project_id is required when scope_level='project'",
            )
        # Raises 403/404 itself if the user can't edit the project.
        project, _role = await get_project_with_access(
            db, str(body.project_id), user.id, Permission.PROJECT_EDIT
        )
        project_id = project.id

    # 3. Per-user install ceiling (counted across active rows for this user only —
    #    team_id is now always NULL on new rows so the limit is per-human, not
    #    per-team-account).
    count_result = await db.execute(
        select(func.count()).where(
            UserMcpConfig.user_id == user.id,
            UserMcpConfig.is_active.is_(True),
        )
    )
    current_count = count_result.scalar() or 0
    if current_count >= settings.mcp_max_servers_per_user:
        raise HTTPException(
            status_code=400,
            detail=f"Maximum of {settings.mcp_max_servers_per_user} connectors per user",
        )

    # 4. Encrypt credentials (static-auth connectors only; OAuth connectors
    #    use services.mcp.oauth_flow).
    encrypted_creds = None
    if body.credentials:
        encrypted_creds = encrypt_credentials(body.credentials)

    # 5. Upsert via the shared helper so both install paths converge on the
    #    same uniqueness tuple. We wrap args in a FlowState-compatible
    #    dataclass shape — the helper only reads these fields.
    flow = FlowState(
        flow_id="static-install",
        user_id=str(user.id),
        server_url=(agent.config or {}).get("url") or f"internal://{agent.slug}",
        auth_server_url="",
        token_endpoint="",
        registration_endpoint=None,
        scope=None,
        code_verifier="n/a",
        code_challenge="n/a",
        client_info={},
        registration_method="dcr",  # unused for static install
        marketplace_agent_id=str(body.marketplace_agent_id),
        scope_level=body.scope_level,
        team_id=None,  # never team-scoped on new installs
        project_id=str(project_id) if project_id else None,
        redirect_uri="",
        resource="",
        protocol_version=None,
    )
    config = await _upsert_user_mcp_config(db, flow=flow)

    # 6. Apply static-auth fields (credentials + capabilities) — these aren't
    #    set by the OAuth helper path.
    server_config = agent.config or {}
    default_capabilities = server_config.get("capabilities", ["tools", "resources", "prompts"])
    if encrypted_creds is not None:
        config.credentials = encrypted_creds
    config.enabled_capabilities = default_capabilities
    config.is_active = True

    await db.commit()
    await db.refresh(config)

    # 7. Test connection (non-fatal). For OAuth connectors we skip this —
    # the row is created without tokens (the user authorizes later from
    # Library → Connectors) so a connect attempt would always raise
    # ReauthRequired and pollute logs.
    if (agent.config or {}).get("auth_type") != "oauth":
        try:
            creds = body.credentials or {}
            await _discover_server(
                agent,
                creds,
                user_mcp_config_id=config.id,
                db=db,
            )
            logger.info("MCP server %s connection verified for user %s", agent.slug, user.id)
        except Exception as e:
            logger.warning(
                "MCP server %s connection test failed (non-fatal): %s",
                agent.slug,
                str(e),
            )

    # Refresh the oauth_connection relationship before building the response.
    # _build_config_response reads config.oauth_connection.tokens_encrypted to
    # populate is_connected; without this refresh the lazy-load tries to issue
    # async IO outside a greenlet → MissingGreenlet.
    await db.refresh(config, ["oauth_connection"])

    return _build_config_response(config, agent)


@router.get("/installed", response_model=list[McpConfigResponse])
async def list_installed_mcp_servers(
    project_id: UUID | None = None,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """List all active MCP (Connector) installations for the current user.

    Connectors are user-identity-bound (#307), so the list is keyed off
    ``user_id`` regardless of which team the user is currently viewing —
    installs follow the user across every team they belong to. Legacy
    team-scoped rows (created before #307) are also included when the
    user is on the team they belong to so we don't hide existing data.

    The MarketplaceAgent join is a LEFT OUTER so custom (BYO) connectors
    that have no ``marketplace_agent_id`` still surface in the list.
    """
    from sqlalchemy import or_ as _or

    team_id = user.default_team_id
    if team_id is not None:
        ownership_filter = _or(
            UserMcpConfig.user_id == user.id,
            UserMcpConfig.team_id == team_id,
        )
    else:
        ownership_filter = UserMcpConfig.user_id == user.id

    from sqlalchemy.orm import selectinload as _selectin

    stmt = (
        select(UserMcpConfig, MarketplaceAgent)
        # Eager-load oauth_connection so _build_config_response can read
        # tokens_encrypted without triggering async lazy-load (MissingGreenlet).
        .options(_selectin(UserMcpConfig.oauth_connection))
        .outerjoin(MarketplaceAgent, UserMcpConfig.marketplace_agent_id == MarketplaceAgent.id)
        .where(
            ownership_filter,
            UserMcpConfig.is_active.is_(True),
        )
        .order_by(UserMcpConfig.created_at.desc())
    )
    # Optional project_id filter for ProjectConnectorPanel — returns only
    # connectors scoped to a specific project (M9, #347).
    if project_id is not None:
        stmt = stmt.where(UserMcpConfig.project_id == project_id)
    result = await db.execute(stmt)
    rows = result.all()

    # Bulk-fetch agent assignments for every config in one query so the
    # Library card's "Add to Agent" button can render its count without a
    # per-card round-trip (otherwise the state vanishes on refresh until
    # the user opens each dropdown).
    config_ids = [c.id for c, _ in rows]
    assignments_by_config: dict[UUID, list[UUID]] = {cid: [] for cid in config_ids}
    if config_ids:
        assign_rows = await db.execute(
            select(AgentMcpAssignment.mcp_config_id, AgentMcpAssignment.agent_id).where(
                AgentMcpAssignment.mcp_config_id.in_(config_ids),
                AgentMcpAssignment.user_id == user.id,
                AgentMcpAssignment.enabled.is_(True),
            )
        )
        for cfg_id, agent_id in assign_rows.fetchall():
            assignments_by_config.setdefault(cfg_id, []).append(agent_id)

    return [
        _build_config_response(
            config, agent, assigned_agent_ids=assignments_by_config.get(config.id, [])
        )
        for config, agent in rows
    ]


@router.get("/installed/{config_id}", response_model=McpConfigResponse)
async def get_installed_mcp_server(
    config_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Get a single MCP server installation (credentials masked)."""
    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)
    agent = await _get_agent_for_config(config.marketplace_agent_id, db)
    return _build_config_response(config, agent)


@router.patch("/installed/{config_id}", response_model=McpConfigResponse)
async def update_installed_mcp_server(
    config_id: UUID,
    body: McpConfigUpdate,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Update credentials or enabled capabilities for an installed MCP server."""
    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)

    if body.credentials is not None:
        config.credentials = encrypt_credentials(body.credentials)
    if body.enabled_capabilities is not None:
        config.enabled_capabilities = body.enabled_capabilities
    if body.is_active is not None:
        config.is_active = body.is_active

    await db.commit()
    await db.refresh(config)

    await _invalidate_mcp_cache(user.id, config.id)

    agent = await _get_agent_for_config(config.marketplace_agent_id, db)
    return _build_config_response(config, agent)


@router.delete("/installed/{config_id}", status_code=204)
async def uninstall_mcp_server(
    config_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Soft-delete an MCP server installation.

    Reinstalling should be a clean slate — so we also drop OAuth tokens, agent
    assignments, per-tool disables, and stored credentials. The row itself
    stays (is_active=False) for audit and to preserve the uniqueness tuple.
    """
    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)

    await db.execute(
        McpOAuthConnection.__table__.delete().where(
            McpOAuthConnection.user_mcp_config_id == config.id
        )
    )
    await db.execute(
        AgentMcpAssignment.__table__.delete().where(AgentMcpAssignment.mcp_config_id == config.id)
    )
    config.credentials = None
    config.disabled_tools = []
    config.needs_reauth = False
    config.last_auth_error = None
    config.is_active = False
    await db.commit()

    await _invalidate_mcp_cache(user.id, config.id)


@router.post("/installed/{config_id}/test", response_model=McpTestResponse)
async def test_mcp_server(
    config_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Test connection to an installed MCP server and return capability counts."""
    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)
    agent = await _get_agent_for_config(config.marketplace_agent_id, db)

    credentials = {}
    if config.credentials:
        try:
            credentials = decrypt_credentials(config.credentials)
        except Exception:
            return McpTestResponse(
                success=False,
                error="Failed to decrypt stored credentials",
            )

    try:
        discovery = await _discover_server(
            agent,
            credentials,
            user_mcp_config_id=config.id,
            db=db,
        )
        return McpTestResponse(
            success=True,
            tool_count=len(discovery.tools),
            resource_count=len(discovery.resources),
            prompt_count=len(discovery.prompts),
        )
    except Exception as e:
        logger.warning("MCP test failed for config %s: %s", config_id, str(e))
        return McpTestResponse(
            success=False,
            error=str(e),
        )


@router.post("/installed/{config_id}/discover", response_model=McpDiscoverResponse)
async def discover_mcp_server(
    config_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Full re-discovery of an MCP server's capabilities (invalidates cache)."""
    from ..services.mcp.oauth_flow import ReauthRequired

    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)
    agent = await _get_agent_for_config(config.marketplace_agent_id, db)

    await _invalidate_mcp_cache(user.id, config.id)

    credentials = {}
    if config.credentials:
        try:
            credentials = decrypt_credentials(config.credentials)
        except Exception:
            raise HTTPException(
                status_code=500,
                detail="Failed to decrypt stored credentials",
            ) from None

    try:
        return await _discover_server(
            agent,
            credentials,
            user_mcp_config_id=config.id,
            db=db,
        )
    except ReauthRequired as e:
        # OAuth connector that hasn't completed the authorize flow (or has
        # an expired/revoked refresh token). Surface a 409 so the UI can
        # render a "connect first" prompt instead of a generic 502.
        raise HTTPException(
            status_code=409,
            detail=(
                "Connector is not connected yet — complete the OAuth flow "
                "from Library → Connectors first."
            ),
        ) from e
    except Exception as e:
        logger.error("MCP discover failed for config %s: %s", config_id, str(e))
        raise HTTPException(
            status_code=502,
            detail=f"Failed to connect to MCP server: {e}",
        ) from e


# ---------------------------------------------------------------------------
# Agent-level MCP server assignment
# ---------------------------------------------------------------------------


@router.get("/installed/{config_id}/agents", response_model=list[UUID])
async def list_connector_agents(
    config_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Return the agent ids this connector is currently assigned to.

    Used by the Library 'Add to Agent' dropdown to render checkmarks next to
    agents the connector already serves so users can toggle multiple
    assignments in one open.
    """
    # Verify ownership of the connector config first.
    await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)
    result = await db.execute(
        select(AgentMcpAssignment.agent_id).where(
            AgentMcpAssignment.mcp_config_id == config_id,
            AgentMcpAssignment.user_id == user.id,
            AgentMcpAssignment.enabled.is_(True),
        )
    )
    return [row[0] for row in result.fetchall()]


@router.post("/installed/{config_id}/assign/{agent_id}", response_model=AgentMcpAssignmentResponse)
async def assign_mcp_to_agent(
    config_id: UUID,
    agent_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Assign an installed MCP server to a specific agent."""
    # Resolve active team for ownership scoping
    team_id = user.default_team_id

    # Verify ownership of the MCP config
    config = await _get_owned_config(config_id, user.id, db, team_id=team_id)

    # Verify the agent exists and is active
    agent_result = await db.execute(
        select(MarketplaceAgent).where(
            MarketplaceAgent.id == agent_id,
            MarketplaceAgent.is_active.is_(True),
        )
    )
    agent = agent_result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    # Check for existing assignment — use OR-based ownership matching
    # _get_owned_config to show both personal and team assignments.
    from sqlalchemy import or_ as _or

    if team_id is not None:
        assignment_ownership = _or(
            AgentMcpAssignment.user_id == user.id,
            AgentMcpAssignment.team_id == team_id,
        )
    else:
        assignment_ownership = AgentMcpAssignment.user_id == user.id
    existing_result = await db.execute(
        select(AgentMcpAssignment).where(
            AgentMcpAssignment.agent_id == agent_id,
            AgentMcpAssignment.mcp_config_id == config_id,
            assignment_ownership,
        )
    )
    existing = existing_result.scalar_one_or_none()

    if existing:
        if existing.enabled:
            # Already assigned and enabled – return as-is
            marketplace_agent = await _get_agent_for_config(config.marketplace_agent_id, db)
            return AgentMcpAssignmentResponse(
                id=existing.id,
                agent_id=existing.agent_id,
                mcp_config_id=existing.mcp_config_id,
                server_name=marketplace_agent.name,
                server_slug=marketplace_agent.slug,
                enabled=existing.enabled,
                added_at=existing.added_at,
            )
        # Re-enable previously disabled assignment
        existing.enabled = True
        await db.commit()
        await db.refresh(existing)
        marketplace_agent = await _get_agent_for_config(config.marketplace_agent_id, db)
        return AgentMcpAssignmentResponse(
            id=existing.id,
            agent_id=existing.agent_id,
            mcp_config_id=existing.mcp_config_id,
            server_name=marketplace_agent.name,
            server_slug=marketplace_agent.slug,
            enabled=existing.enabled,
            added_at=existing.added_at,
        )

    assignment = AgentMcpAssignment(
        agent_id=agent_id,
        mcp_config_id=config_id,
        user_id=user.id,
        team_id=team_id,
        enabled=True,
    )
    db.add(assignment)
    await db.commit()
    await db.refresh(assignment)

    marketplace_agent = await _get_agent_for_config(config.marketplace_agent_id, db)
    return AgentMcpAssignmentResponse(
        id=assignment.id,
        agent_id=assignment.agent_id,
        mcp_config_id=assignment.mcp_config_id,
        server_name=marketplace_agent.name,
        server_slug=marketplace_agent.slug,
        enabled=assignment.enabled,
        added_at=assignment.added_at,
    )


@router.delete("/installed/{config_id}/assign/{agent_id}", status_code=204)
async def unassign_mcp_from_agent(
    config_id: UUID,
    agent_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Remove an MCP server from a specific agent."""
    # Resolve active team for ownership scoping — OR-based to match
    # _get_owned_config and avoid hiding personal assignments.
    from sqlalchemy import or_ as _or

    team_id = user.default_team_id
    if team_id is not None:
        ownership_filter = _or(
            AgentMcpAssignment.user_id == user.id,
            AgentMcpAssignment.team_id == team_id,
        )
    else:
        ownership_filter = AgentMcpAssignment.user_id == user.id

    result = await db.execute(
        select(AgentMcpAssignment).where(
            AgentMcpAssignment.agent_id == agent_id,
            AgentMcpAssignment.mcp_config_id == config_id,
            ownership_filter,
        )
    )
    assignment = result.scalar_one_or_none()

    if not assignment:
        raise HTTPException(status_code=404, detail="MCP assignment not found")

    await db.delete(assignment)
    await db.commit()


@router.get("/agent/{agent_id}/servers", response_model=list[AgentMcpAssignmentResponse])
async def get_agent_mcp_servers(
    agent_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """List MCP servers assigned to a specific agent."""
    # Resolve active team for ownership scoping — OR-based to match
    # _get_owned_config and avoid hiding personal assignments.
    from sqlalchemy import or_ as _or

    team_id = user.default_team_id
    if team_id is not None:
        ownership_filter = _or(
            AgentMcpAssignment.user_id == user.id,
            AgentMcpAssignment.team_id == team_id,
        )
    else:
        ownership_filter = AgentMcpAssignment.user_id == user.id

    # Verify agent exists
    agent_result = await db.execute(
        select(MarketplaceAgent).where(
            MarketplaceAgent.id == agent_id,
            MarketplaceAgent.is_active.is_(True),
        )
    )
    if not agent_result.scalar_one_or_none():
        raise HTTPException(status_code=404, detail="Agent not found")

    result = await db.execute(
        select(AgentMcpAssignment, UserMcpConfig, MarketplaceAgent)
        .join(UserMcpConfig, AgentMcpAssignment.mcp_config_id == UserMcpConfig.id)
        .join(MarketplaceAgent, UserMcpConfig.marketplace_agent_id == MarketplaceAgent.id)
        .where(
            AgentMcpAssignment.agent_id == agent_id,
            ownership_filter,
            AgentMcpAssignment.enabled.is_(True),
            UserMcpConfig.is_active.is_(True),
        )
    )
    rows = result.all()

    return [
        AgentMcpAssignmentResponse(
            id=assignment.id,
            agent_id=assignment.agent_id,
            mcp_config_id=assignment.mcp_config_id,
            server_name=marketplace_agent.name,
            server_slug=marketplace_agent.slug,
            enabled=assignment.enabled,
            added_at=assignment.added_at,
        )
        for assignment, _config, marketplace_agent in rows
    ]


# ---------------------------------------------------------------------------
# OAuth-connector specific endpoints (catalog, per-tool toggles, overrides,
# reconnect) — added in #287 / Phase 3.
# ---------------------------------------------------------------------------


# Pydantic models for the new endpoints ------------------------------------


class CatalogEntry(BaseModel):
    """One row in GET /api/mcp/catalog — a published MCP server."""

    id: UUID
    slug: str
    name: str
    description: str
    icon: str | None = None
    icon_url: str | None = None
    category: str | None = None
    config: dict = Field(default_factory=dict)


class DisabledToolsUpdate(BaseModel):
    """Body for PATCH /api/mcp/installed/{id}/tools."""

    disabled_tools: list[str]


class OverrideRequest(BaseModel):
    """Body for POST /api/mcp/installed/{id}/override."""

    project_id: UUID


class OverrideResponse(BaseModel):
    id: UUID
    parent_config_id: UUID
    project_id: UUID
    scope_level: str


class ReconnectResponse(BaseModel):
    authorize_url: str
    flow_id: str


async def _require_project_member(user: Any, project_id: UUID, db: AsyncSession) -> None:
    """Verify the user has edit-level access to the target project.

    Raises 403 if the user cannot manage connectors at the project scope.
    """
    from ..permissions import Permission, get_project_with_access

    await get_project_with_access(db, str(project_id), user.id, Permission.PROJECT_EDIT)


@router.get("/catalog", response_model=list[CatalogEntry])
async def get_mcp_catalog(
    db: AsyncSession = Depends(get_db),
    user=Depends(current_active_user),  # noqa: ARG001 — require auth, no per-user filter
):
    """List all published MCP servers from the marketplace."""
    result = await db.execute(
        select(MarketplaceAgent)
        .where(
            MarketplaceAgent.item_type == "mcp_server",
            MarketplaceAgent.is_active.is_(True),
            MarketplaceAgent.is_published.is_(True),
        )
        .order_by(MarketplaceAgent.name.asc())
    )
    rows = list(result.scalars().all())
    return [
        CatalogEntry(
            id=row.id,
            slug=row.slug,
            name=row.name,
            description=row.description,
            icon=row.icon,
            icon_url=row.avatar_url,
            category=row.category,
            config=row.config or {},
        )
        for row in rows
    ]


@router.patch("/installed/{config_id}/tools", response_model=McpConfigResponse)
async def update_disabled_tools(
    config_id: UUID,
    body: DisabledToolsUpdate,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Update the ``disabled_tools`` filter on an installed MCP config.

    The list entries are prefixed tool names (``mcp__{slug}__{tool}``) — the
    same form :func:`services.mcp.manager.get_user_mcp_context` compares
    against when bridging. Invalidates the Redis schema cache so the change
    takes effect on the next agent run.
    """
    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)
    # Normalize to a unique, sorted list.
    config.disabled_tools = sorted(set(body.disabled_tools))
    await db.commit()
    await db.refresh(config)
    await _invalidate_mcp_cache(user.id, config.id)
    agent = None
    if config.marketplace_agent_id:
        agent = await _get_agent_for_config(config.marketplace_agent_id, db)
    return _build_config_response(config, agent)


@router.post("/installed/{config_id}/override", response_model=OverrideResponse)
async def override_config_for_project(
    config_id: UUID,
    body: OverrideRequest,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Clone a user/team-scoped config into a project-scoped row.

    The resulting project row's ``parent_config_id`` references the source
    for the UI ("Inherited from team"). Reconnection flow is left to the
    client — the new row is inactive-but-present until the user completes
    OAuth (or the override copies credentials for static auth).
    """
    await _require_project_member(user, body.project_id, db)

    source = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)
    if source.scope_level == "project":
        raise HTTPException(status_code=400, detail="Cannot override a project-scoped config")

    # Already overridden? Scope by user to prevent cross-tenant collisions.
    existing = (
        await db.execute(
            select(UserMcpConfig).where(
                UserMcpConfig.parent_config_id == source.id,
                UserMcpConfig.project_id == body.project_id,
                UserMcpConfig.user_id == user.id,
                UserMcpConfig.is_active.is_(True),
            )
        )
    ).scalar_one_or_none()
    if existing:
        return OverrideResponse(
            id=existing.id,
            parent_config_id=source.id,
            project_id=body.project_id,
            scope_level=existing.scope_level,
        )

    clone = UserMcpConfig(
        user_id=user.id,
        team_id=None,
        project_id=body.project_id,
        marketplace_agent_id=source.marketplace_agent_id,
        credentials=source.credentials,
        enabled_capabilities=source.enabled_capabilities,
        disabled_tools=source.disabled_tools,
        scope_level="project",
        parent_config_id=source.id,
        is_active=True,
    )
    db.add(clone)
    await db.commit()
    await db.refresh(clone)
    return OverrideResponse(
        id=clone.id,
        parent_config_id=source.id,
        project_id=body.project_id,
        scope_level="project",
    )


@router.delete("/installed/{config_id}/override", status_code=204)
async def remove_project_override(
    config_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Remove a project-scoped override. The row must be project-scoped."""
    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)
    if config.scope_level != "project":
        raise HTTPException(
            status_code=400,
            detail="Only project-scoped overrides can be removed via this endpoint",
        )
    config.is_active = False
    await db.commit()
    await _invalidate_mcp_cache(user.id, config.id)


@router.post("/installed/{config_id}/disconnect", status_code=204)
async def disconnect_mcp_config(
    config_id: UUID,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Sign out of an OAuth connector — drop the stored tokens/client_info.

    Unlike Uninstall, this keeps the ``UserMcpConfig`` row (and any per-tool
    permissions / agent assignments) so the user can click Connect/Reconnect
    later without re-attaching it to each agent. For static-auth connectors
    we clear the encrypted credentials instead.
    """
    from ..models import McpOAuthConnection as _McpOAuthConnection

    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)

    # Drop the OAuth row if there is one. CASCADE on user_mcp_config_id
    # would handle this on uninstall, but we want the config row to stay.
    await db.execute(
        _McpOAuthConnection.__table__.delete().where(
            _McpOAuthConnection.user_mcp_config_id == config.id
        )
    )
    # For static-auth connectors, wipe the encrypted env-var blob too so
    # the card flips to "Not connected" uniformly.
    config.credentials = None
    # Disconnect clears the reauth flag — the connector is now in the "not
    # connected" state, not "connected but stale."
    config.needs_reauth = False
    config.last_auth_error = None
    await db.commit()
    await _invalidate_mcp_cache(user.id, config.id)


@router.post("/installed/{config_id}/reconnect", response_model=ReconnectResponse)
async def reconnect_mcp_config(
    config_id: UUID,
    request: Request,
    user=Depends(current_active_user),
    db: AsyncSession = Depends(get_db),
):
    """Start a fresh OAuth flow for an existing connector.

    Used when the refresh token has been revoked or expired. Reuses the
    original ``MarketplaceAgent.config`` + ``registration_method``.
    """
    from ..services.mcp.oauth_flow import OAuthFlowError, start_oauth_flow

    config = await _get_owned_config(config_id, user.id, db, team_id=user.default_team_id)

    # Pull provider metadata from the original install.
    endpoint_overrides: dict | None = None
    if config.marketplace_agent_id:
        agent = await _get_agent_for_config(config.marketplace_agent_id, db)
        agent_cfg = agent.config or {}
        server_url = agent_cfg.get("url")
        registration_method = agent_cfg.get("registration_method", "dcr")
        # Providers that don't serve RFC 8414 discovery (e.g. GitHub's
        # platform_app) can ship hardcoded endpoints in their catalog config.
        endpoint_overrides = agent_cfg.get("oauth_endpoints")
    else:
        # Custom connector — get server_url from paired oauth_connection.
        oauth_row = config.oauth_connection
        if not oauth_row:
            raise HTTPException(
                status_code=400,
                detail="Custom connector has no prior OAuth connection to reuse",
            )
        server_url = oauth_row.server_url
        registration_method = oauth_row.registration_method

    if not server_url:
        raise HTTPException(status_code=400, detail="No server URL resolved for reconnect")

    redirect_uri = build_mcp_oauth_callback_url(request, get_settings())

    try:
        result = await start_oauth_flow(
            db=db,
            user_id=user.id,
            server_url=server_url,
            registration_method=registration_method,
            redirect_uri=redirect_uri,
            marketplace_agent_id=config.marketplace_agent_id,
            scope_level=config.scope_level,
            team_id=config.team_id,
            project_id=config.project_id,
            endpoint_overrides=endpoint_overrides,
        )
    except OAuthFlowError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc

    return ReconnectResponse(authorize_url=result.authorize_url, flow_id=result.flow_id)
