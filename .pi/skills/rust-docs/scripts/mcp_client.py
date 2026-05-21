#!/usr/bin/env python3
"""
MCP client for rust-docs-mcp server.
Communicates with the MCP server over stdio using JSON-RPC.
"""
import json
import subprocess
import sys
import os
import uuid
import time

RUST_DOCS_BIN = os.environ.get("RUST_DOCS_MCP_BIN", "rust-docs-mcp")
CACHE_DIR = os.environ.get("RUST_DOCS_MCP_CACHE_DIR", "")
MCP_TIMEOUT = int(os.environ.get("RUST_DOCS_MCP_TIMEOUT", "60"))


def find_server():
    """Find the rust-docs-mcp binary."""
    # Try PATH first
    for path_dir in os.environ.get("PATH", "").split(":"):
        candidate = os.path.join(path_dir, RUST_DOCS_BIN)
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    # Check cargo bin
    cargo_bin = os.path.expanduser("~/.cargo/bin/rust-docs-mcp")
    if os.path.isfile(cargo_bin):
        return cargo_bin
    return RUST_DOCS_BIN


def make_request(req_id, method, params=None):
    """Build a JSON-RPC request."""
    msg = {"jsonrpc": "2.0", "id": req_id, "method": method}
    if params:
        msg["params"] = params
    return msg


def parse_response(line):
    """Parse a JSON-RPC response line (could be request, response, or notification)."""
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        return None


class MCPClient:
    """Minimal MCP client that communicates over stdio."""

    def __init__(self, bin_path):
        self.bin_path = bin_path
        self.proc = None
        self.req_id = 0
        self.server_caps = None
        self.pending = {}

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def start(self):
        """Start the MCP server process."""
        args = [self.bin_path]
        if CACHE_DIR:
            args.extend(["--cache-dir", CACHE_DIR])

        self.proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        # Initialize
        caps = self._call("initialize", {
            "protocolVersion": "0.1.0",
            "capabilities": {},
            "clientInfo": {"name": "pi-rust-docs", "version": "1.0"},
        })
        self.server_caps = caps
        # Send initialized notification
        self._notify("notifications/initialized")
        return caps

    def close(self):
        """Close the server process."""
        if self.proc:
            try:
                self.proc.stdin.close()
                self.proc.wait(timeout=5)
            except Exception:
                self.proc.kill()
            self.proc = None

    def _notify(self, method, params=None):
        """Send a JSON-RPC notification (no response expected)."""
        msg = {"jsonrpc": "2.0", "method": method}
        if params:
            msg["params"] = params
        self._send(msg)

    def _call(self, method, params=None):
        """Send a JSON-RPC request and wait for response."""
        self.req_id += 1
        req_id = self.req_id
        msg = make_request(req_id, method, params)
        self._send(msg)
        return self._recv(req_id)

    def _send(self, msg):
        """Send a JSON-RPC message."""
        line = json.dumps(msg, ensure_ascii=False)
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()

    def _recv(self, expected_id=None, timeout=MCP_TIMEOUT):
        """Receive a JSON-RPC response."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                # Check if process died
                ret = self.proc.poll()
                if ret is not None:
                    stderr_out = self.proc.stderr.read()
                    raise RuntimeError(
                        f"MCP server exited with code {ret}\nstderr: {stderr_out[:500]}"
                    )
                continue

            parsed = parse_response(line.strip())
            if parsed is None:
                continue

            # Check for errors
            if "error" in parsed and parsed.get("id") == expected_id:
                err = parsed["error"]
                raise RuntimeError(
                    f"MCP error: {err.get('message', 'unknown')} "
                    f"(code: {err.get('code', -1)})"
                )

            # If this is a response to our request
            if expected_id is not None and parsed.get("id") == expected_id:
                return parsed.get("result")

        raise TimeoutError(f"MCP request timed out after {timeout}s")

    def list_tools(self):
        """List available tools."""
        return self._call("tools/list")

    def call_tool(self, name, arguments=None):
        """Call a tool with arguments."""
        params = {"name": name}
        if arguments:
            params["arguments"] = arguments
        return self._call("tools/call", params)


def main():
    if len(sys.argv) < 2:
        print("Usage: mcp_client.py <command> [args...]", file=sys.stderr)
        sys.exit(1)

    command = sys.argv[1]
    args = sys.argv[2:]

    bin_path = find_server()

    try:
        with MCPClient(bin_path) as client:
            client.start()
            result = handle_command(client, command, args)
            if result is not None:
                print(result)
    except Exception as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)


def handle_command(client, command, args):
    """Route command to the appropriate MCP tool call."""

    if command == "cache":
        return cmd_cache(client, args)
    elif command == "list-cached":
        return cmd_list_cached(client, args)
    elif command == "remove":
        return cmd_remove(client, args)
    elif command == "list-items":
        return cmd_list_items(client, args)
    elif command == "search":
        return cmd_search(client, args)
    elif command == "search-preview":
        return cmd_search_preview(client, args)
    elif command == "search-fuzzy":
        return cmd_search_fuzzy(client, args)
    elif command == "get-details":
        return cmd_get_details(client, args)
    elif command == "get-docs":
        return cmd_get_docs(client, args)
    elif command == "get-source":
        return cmd_get_source(client, args)
    elif command == "deps":
        return cmd_deps(client, args)
    elif command == "structure":
        return cmd_structure(client, args)
    else:
        raise ValueError(f"Unknown command: {command}")


def parse_kwargs(args):
    """Parse CLI args into positional and keyword arguments."""
    pos = []
    kwargs = {}
    i = 0
    while i < len(args):
        if args[i].startswith("--"):
            key = args[i][2:].replace("-", "_")
            if i + 1 < len(args) and not args[i + 1].startswith("--"):
                kwargs[key] = args[i + 1]
                i += 2
            else:
                kwargs[key] = True
                i += 1
        else:
            pos.append(args[i])
            i += 1
    return pos, kwargs


def fmt_json(obj):
    """Format JSON output nicely."""
    return json.dumps(obj, indent=2, ensure_ascii=False)


# --- Command implementations ---

def cmd_cache(client, args):
    """Cache a crate from various sources."""
    pos, kwargs = parse_kwargs(args)
    if not pos:
        raise ValueError("Usage: cache <crate-name> [--version V] [--source cratesio|github|local] [--github-url URL] [--tag T] [--branch B] [--path P]")

    crate_name = pos[0]
    source_type = kwargs.get("source", "cratesio")
    version = kwargs.get("version")

    if source_type == "cratesio":
        if not version:
            raise ValueError("--version is required for crates.io source")
        result = client.call_tool("cache_crate_from_cratesio", {
            "crate_name": crate_name,
            "version": version,
        })
    elif source_type == "github":
        github_url = kwargs.get("github_url")
        if not github_url:
            raise ValueError("--github-url is required for GitHub source")
        args_dict = {"crate_name": crate_name, "github_url": github_url}
        if kwargs.get("branch"):
            args_dict["branch"] = kwargs["branch"]
        if kwargs.get("tag"):
            args_dict["tag"] = kwargs["tag"]
        result = client.call_tool("cache_crate_from_github", args_dict)
    elif source_type == "local":
        path = kwargs.get("path")
        if not path:
            raise ValueError("--path is required for local source")
        args_dict = {"crate_name": crate_name, "path": path}
        if version:
            args_dict["version"] = version
        result = client.call_tool("cache_crate_from_local", args_dict)
    else:
        raise ValueError(f"Unknown source type: {source_type}")

    return fmt_json(result)


def cmd_list_cached(client, args):
    """List all cached crates."""
    result = client.call_tool("list_cached_crates")
    return fmt_json(result)


def cmd_remove(client, args):
    """Remove a cached crate."""
    pos, kwargs = parse_kwargs(args)
    if not pos:
        raise ValueError("Usage: remove <crate-name> [--version V]")

    crate_name = pos[0]
    params = {"crate_name": crate_name}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]

    result = client.call_tool("remove_crate", params)
    return fmt_json(result)


def cmd_list_items(client, args):
    """List items in a crate."""
    pos, kwargs = parse_kwargs(args)
    if not pos:
        raise ValueError("Usage: list-items <crate-name> [--version V] [--kind K] [--query Q]")

    crate_name = pos[0]
    params = {"crate_name": crate_name}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]
    if kwargs.get("kind"):
        params["kind"] = kwargs["kind"]
    if kwargs.get("query"):
        params["query"] = kwargs["query"]

    result = client.call_tool("list_crate_items", params)
    return fmt_json(result)


def cmd_search(client, args):
    """Full search."""
    pos, kwargs = parse_kwargs(args)
    if len(pos) < 2:
        raise ValueError("Usage: search <crate-name> <query> [--version V]")

    crate_name = pos[0]
    query = pos[1]
    params = {"crate_name": crate_name, "query": query}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]

    result = client.call_tool("search_items", params)
    return fmt_json(result)


def cmd_search_preview(client, args):
    """Lightweight search preview."""
    pos, kwargs = parse_kwargs(args)
    if len(pos) < 2:
        raise ValueError("Usage: search-preview <crate-name> <query> [--version V]")

    crate_name = pos[0]
    query = pos[1]
    params = {"crate_name": crate_name, "query": query}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]

    result = client.call_tool("search_items_preview", params)
    return fmt_json(result)


def cmd_search_fuzzy(client, args):
    """Fuzzy search."""
    pos, kwargs = parse_kwargs(args)
    if not pos:
        raise ValueError("Usage: search-fuzzy <query>")

    query = pos[0]
    params = {"query": query}

    result = client.call_tool("search_items_fuzzy", params)
    return fmt_json(result)


def cmd_get_details(client, args):
    """Get item details."""
    pos, kwargs = parse_kwargs(args)
    if len(pos) < 2:
        raise ValueError("Usage: get-details <crate-name> <item-path> [--version V]")

    crate_name = pos[0]
    item_path = pos[1]
    params = {"crate_name": crate_name, "item_path": item_path}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]

    result = client.call_tool("get_item_details", params)
    return fmt_json(result)


def cmd_get_docs(client, args):
    """Get item documentation."""
    pos, kwargs = parse_kwargs(args)
    if len(pos) < 2:
        raise ValueError("Usage: get-docs <crate-name> <item-path> [--version V]")

    crate_name = pos[0]
    item_path = pos[1]
    params = {"crate_name": crate_name, "item_path": item_path}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]

    result = client.call_tool("get_item_docs", params)
    return fmt_json(result)


def cmd_get_source(client, args):
    """Get item source code."""
    pos, kwargs = parse_kwargs(args)
    if len(pos) < 2:
        raise ValueError("Usage: get-source <crate-name> <item-path> [--version V] [--context N]")

    crate_name = pos[0]
    item_path = pos[1]
    params = {"crate_name": crate_name, "item_path": item_path}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]
    if kwargs.get("context"):
        params["context_lines"] = int(kwargs["context"])

    result = client.call_tool("get_item_source", params)
    return fmt_json(result)


def cmd_deps(client, args):
    """Get dependencies."""
    pos, kwargs = parse_kwargs(args)
    if not pos:
        raise ValueError("Usage: deps <crate-name> [--version V] [--depth direct|transitive]")

    crate_name = pos[0]
    params = {"crate_name": crate_name}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]
    if kwargs.get("depth"):
        params["depth"] = kwargs["depth"]

    result = client.call_tool("get_dependencies", params)
    return fmt_json(result)


def cmd_structure(client, args):
    """Get module structure."""
    pos, kwargs = parse_kwargs(args)
    if not pos:
        raise ValueError("Usage: structure <crate-name> [--version V]")

    crate_name = pos[0]
    params = {"crate_name": crate_name}
    if kwargs.get("version"):
        params["version"] = kwargs["version"]

    result = client.call_tool("structure", params)
    return fmt_json(result)


if __name__ == "__main__":
    main()
