"""Helpers for MCP OAuth redirect URLs."""

from __future__ import annotations

from typing import Any

from fastapi import Request


def build_mcp_oauth_callback_url(request: Request, settings: Any) -> str:
    """Build the callback URL providers should redirect back to."""
    request_origin = f"{request.url.scheme}://{request.url.netloc}"
    if getattr(settings, "is_desktop_mode", False):
        base = request_origin
    else:
        base = getattr(settings, "public_base_url", "") or request_origin
    return f"{base.rstrip('/')}/api/mcp/oauth/callback"
