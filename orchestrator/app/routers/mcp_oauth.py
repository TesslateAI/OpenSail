"""OAuth initial-flow endpoints for MCP connectors.

Flow: frontend POSTs to ``/start`` → gets an authorize URL → opens it in a
popup → the provider redirects the user to ``/callback`` with ``?code=...&state=...``
→ the callback exchanges the code for tokens and closes the popup by posting a
message back to the opener window.

Runtime refresh-on-401 is handled separately by the MCP SDK's
``OAuthClientProvider`` via :class:`PostgresTokenStorage`; this router is only
for the initial connect and the manual reconnect flow.
"""

from __future__ import annotations

import html as html_mod
import logging
from typing import Any
from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import HTMLResponse
from pydantic import BaseModel, HttpUrl, model_validator
from sqlalchemy.ext.asyncio import AsyncSession

from ..config import get_settings
from ..database import get_db
from ..models import User
from ..services.mcp.oauth_flow import (
    OAuthFlowError,
    OAuthTokenError,
    complete_oauth_flow,
    get_flow_status,
    resolve_catalog_server,
    start_oauth_flow,
)
from ..services.mcp.oauth_redirect import build_mcp_oauth_callback_url
from ..users import current_active_user

logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/mcp/oauth", tags=["mcp-oauth"])


# ---------------------------------------------------------------------------
# Request / response schemas
# ---------------------------------------------------------------------------


class StartOAuthRequest(BaseModel):
    """Body for ``POST /api/mcp/oauth/start``.

    Either ``marketplace_agent_slug`` OR ``server_url`` must be set.
    """

    marketplace_agent_slug: str | None = None
    server_url: HttpUrl | None = None
    registration_method: str = "dcr"
    scope_level: str = "user"  # team | user | project
    team_id: UUID | None = None
    project_id: UUID | None = None
    byo_client_id: str | None = None
    byo_client_secret: str | None = None
    scope: str | None = None

    @model_validator(mode="after")
    def _platform_app_requires_catalog_slug(self) -> StartOAuthRequest:
        """``platform_app`` may only be used with catalog entries.

        Without this guard an attacker could send an arbitrary ``server_url``
        with ``registration_method='platform_app'`` and exfiltrate Tesslate's
        platform ``client_secret`` to their own token endpoint.
        """
        if self.registration_method == "platform_app" and not self.marketplace_agent_slug:
            raise ValueError(
                "platform_app registration requires marketplace_agent_slug — "
                "custom server URLs must use 'dcr' or 'byo'"
            )
        return self


class StartOAuthResponse(BaseModel):
    authorize_url: str
    flow_id: str


class OAuthStatusResponse(BaseModel):
    status: str  # "pending" | "success" | "error" | "unknown"
    config_id: str | None = None
    error: str | None = None


# ---------------------------------------------------------------------------
# Endpoints
# ---------------------------------------------------------------------------


@router.post("/start", response_model=StartOAuthResponse)
async def start_oauth(
    body: StartOAuthRequest,
    request: Request,
    db: AsyncSession = Depends(get_db),
    user: User = Depends(current_active_user),
) -> StartOAuthResponse:
    """Begin an OAuth flow for a catalog or custom MCP server."""

    if body.registration_method not in ("dcr", "byo", "platform_app"):
        raise HTTPException(status_code=400, detail="invalid registration_method")
    if body.scope_level not in ("user", "project"):
        # Team scope is intentionally unsupported — OAuth identities belong to
        # individual users and can't be shared across team members. See
        # issue #307 for the full rationale.
        raise HTTPException(
            status_code=400,
            detail=(
                "Unsupported scope_level. Connectors install at 'user' (default) "
                "or 'project' scope. Team-scope install is not supported because "
                "OAuth identities cannot be shared across team members."
            ),
        )
    if body.scope_level == "project":
        if not body.project_id:
            raise HTTPException(status_code=400, detail="project_id required for project scope")
        # RBAC: caller must be able to edit the target project.
        from ..permissions import Permission, get_project_with_access

        await get_project_with_access(db, str(body.project_id), user.id, Permission.PROJECT_EDIT)

    marketplace_agent_id: UUID | None = None
    server_url: str
    endpoint_overrides: dict | None = None

    if body.marketplace_agent_slug:
        try:
            agent, server_url = await resolve_catalog_server(db, body.marketplace_agent_slug)
        except OAuthFlowError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        marketplace_agent_id = agent.id
        endpoint_overrides = (agent.config or {}).get("oauth_endpoints")
    elif body.server_url:
        server_url = str(body.server_url)
    else:
        raise HTTPException(
            status_code=400,
            detail="one of marketplace_agent_slug or server_url is required",
        )

    redirect_uri = _callback_url(request)

    try:
        result = await start_oauth_flow(
            db=db,
            user_id=user.id,
            server_url=server_url,
            registration_method=body.registration_method,  # type: ignore[arg-type]
            redirect_uri=redirect_uri,
            marketplace_agent_id=marketplace_agent_id,
            scope_level=body.scope_level,
            team_id=None,  # team-scope install dropped (#307)
            project_id=body.project_id,
            byo_client_id=body.byo_client_id,
            byo_client_secret=body.byo_client_secret,
            scope=body.scope,
            endpoint_overrides=endpoint_overrides,
        )
    except OAuthFlowError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc

    return StartOAuthResponse(authorize_url=result.authorize_url, flow_id=result.flow_id)


@router.get("/callback")
async def oauth_callback(
    code: str | None = Query(default=None),
    state: str | None = Query(default=None),
    error: str | None = Query(default=None),
    error_description: str | None = Query(default=None),
    db: AsyncSession = Depends(get_db),
) -> HTMLResponse:
    """Public endpoint that the provider redirects to.

    Authentication is bound via the ``state`` token (single-use, stored in Redis
    with the originating user_id). Rendered HTML posts a message back to the
    opener window and closes itself.
    """
    if error:
        msg = error_description or error
        logger.warning("MCP OAuth callback error: %s", msg)
        return _callback_html(status_="error", message=msg)
    if not code or not state:
        return _callback_html(status_="error", message="missing code/state")

    try:
        config = await complete_oauth_flow(db=db, state=state, code=code)
        await db.commit()
    except OAuthTokenError as exc:
        logger.warning("MCP OAuth token exchange failed: %s", exc)
        await db.rollback()
        return _callback_html(status_="error", message="Token exchange failed")
    except OAuthFlowError as exc:
        logger.warning("MCP OAuth flow failed: %s", exc)
        await db.rollback()
        return _callback_html(status_="error", message=str(exc))
    except Exception:  # pragma: no cover - unexpected
        logger.exception("MCP OAuth callback failed")
        await db.rollback()
        return _callback_html(status_="error", message="Unexpected error")

    return _callback_html(status_="success", config_id=str(config.id))


@router.get("/status/{flow_id}", response_model=OAuthStatusResponse)
async def oauth_status(
    flow_id: str,
    user: User = Depends(current_active_user),
) -> OAuthStatusResponse:
    """Poll for flow status as a postMessage fallback when popups are blocked."""
    status_dict = await get_flow_status(flow_id)
    if not status_dict:
        return OAuthStatusResponse(status="unknown")
    return OAuthStatusResponse(
        status=status_dict.get("status", "unknown"),
        config_id=status_dict.get("config_id"),
        error=status_dict.get("error"),
    )


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _callback_url(request: Request) -> str:
    """Build the absolute URL of this router's /callback endpoint."""
    return build_mcp_oauth_callback_url(request, get_settings())


def _callback_html(
    *,
    status_: str,
    message: str = "",
    config_id: str | None = None,
) -> HTMLResponse:
    """Render a tiny HTML page that postMessages the status and closes itself."""
    settings = get_settings()
    # frontend_origin may be a comma-separated list (e.g. the dev default
    # "http://localhost,http://localhost:5173"). The callback script picks
    # the entry that matches the opener's document.referrer so popup auth
    # works across both Vite dev server (:5173) and Traefik-proxied frontend
    # (:80). Never falls back to "*" — the payload includes config_id.
    raw = getattr(settings, "frontend_origin", "") or ""
    origins = [o.strip() for o in raw.split(",") if o.strip()]
    payload: dict[str, Any] = {"type": "mcp-oauth", "status": status_}
    if config_id:
        payload["config_id"] = config_id
    if message:
        payload["message"] = message

    safe_message = html_mod.escape(message or ("Connected!" if status_ == "success" else "Failed"))
    payload_js = _safe_json_js(payload)
    origins_js = _safe_json_js(origins)
    body = f"""
<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>MCP Connector</title>
<style>body{{font-family:-apple-system,Segoe UI,Roboto,sans-serif;display:flex;flex-direction:column;align-items:center;justify-content:center;height:100vh;margin:0;background:#0b0b0e;color:#fafafa}}h1{{margin:8px 0;font-size:20px}}p{{color:#888;margin:0}}</style>
</head>
<body>
  <h1>{"Connected" if status_ == "success" else "Connection Failed"}</h1>
  <p>{safe_message}</p>
  <p style="margin-top:12px;color:#555">This window will close automatically.</p>
  <script>
    (function() {{
      var payload = {payload_js};
      var origins = {origins_js};
      // Pick the configured origin that matches the opener page.
      var refOrigin = "";
      try {{
        if (document.referrer) refOrigin = new URL(document.referrer).origin;
      }} catch (e) {{ /* ignore */ }}
      var targetOrigin = origins.indexOf(refOrigin) >= 0 ? refOrigin : (origins[0] || "");
      try {{
        if (window.opener && targetOrigin) {{
          window.opener.postMessage(payload, targetOrigin);
        }}
      }} catch (e) {{ /* ignore */ }}
      setTimeout(function() {{ window.close(); }}, 800);
    }})();
  </script>
</body>
</html>
"""
    return HTMLResponse(content=body, status_code=200)


def _safe_json_js(payload: Any) -> str:
    """Render a Python value as JSON suitable for embedding in a <script>."""
    import json as _json

    # Escape </script> and line separators that would break out of <script>.
    return (
        _json.dumps(payload)
        .replace("</", "<\\/")
        .replace("\u2028", "\\u2028")
        .replace("\u2029", "\\u2029")
    )
