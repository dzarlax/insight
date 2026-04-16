"""Bitbucket Cloud authentication helpers."""

import base64


def auth_headers(token: str, username: str = "") -> dict:
    """Build authentication headers for Bitbucket Cloud API requests.

    Both personal API tokens and workspace access tokens use Basic Auth.
    Personal tokens require username:token. Workspace tokens use
    an arbitrary username (defaults to "x-token-auth") with the token as password.
    """
    if not username:
        username = "x-token-auth"
    credentials = base64.b64encode(f"{username}:{token}".encode()).decode()
    auth_value = f"Basic {credentials}"

    return {
        "Authorization": auth_value,
        "Accept": "application/json",
        "User-Agent": "insight-bitbucket-cloud-connector/1.0",
    }
