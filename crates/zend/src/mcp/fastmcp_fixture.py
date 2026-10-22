# /// script
# requires-python = ">=3.10"
# dependencies = ["fastmcp==4.1.3"]
# ///
"""Reference interoperability fixture (not a production dependency).

Run: uv run crates/zend/src/mcp/fastmcp_fixture.py
Then: MCP_FASTMCP_URL=http://127.0.0.1:18765/mcp cargo test --release -p zend \
      fastmcp_reference_interoperability -- --ignored
Also accepts --stdio for an owner-configured local MCP test.
"""
import sys
import base64
from fastmcp.utilities.types import Image
from fastmcp import FastMCP, Context

mcp = FastMCP("zenbot phase 5 reference")


@mcp.tool
def echo(message: str) -> dict:
    """Return text and structured data for a supplied message."""
    return {"message": message, "source": "FastMCP"}


@mcp.tool
def image() -> Image:
    """Return a real one-pixel PNG for multimodal interoperability."""
    return Image(data=base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jF3sAAAAASUVORK5CYII="), format="png")


@mcp.tool
async def confirm(ctx: Context) -> str:
    """Ask the owner a harmless form question; useful for the owner pending-request flow."""
    answer = await ctx.elicit("Allow this fixture to say hello?", response_type=bool)
    return "hello" if answer.action == "accept" and answer.data else answer.action


@mcp.resource("fixture://greeting")
def greeting() -> str:
    return "hello from FastMCP"


@mcp.resource("fixture://items/{item}")
def item_resource(item: str) -> str:
    return f"item {item}"


@mcp.prompt
def greet(name: str) -> str:
    return f"Greet {name} using the connected service."


if __name__ == "__main__":
    if "--stdio" in sys.argv:
        mcp.run(transport="stdio")
    else:
        mcp.run(transport="http", host="127.0.0.1", port=18765)
