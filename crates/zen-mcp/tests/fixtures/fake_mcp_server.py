#!/usr/bin/env python3
"""
Fake MCP Server for testing zen-mcp integration.
Implements a minimal MCP server over stdio that responds to initialize,
notifications/initialized, tools/list, and tools/call.
"""

import json
import sys
import os

def log(msg):
    """Log to stderr to avoid interfering with JSON-RPC over stdout."""
    print(f"[fake-mcp-server] {msg}", file=sys.stderr, flush=True)

def handle_initialize(request_id):
    """Handle initialize request."""
    log("Handling initialize")
    response = {
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "fake-mcp-server",
                "version": "0.1.0"
            }
        }
    }
    print(json.dumps(response), flush=True)

def handle_initialized():
    """Handle notifications/initialized."""
    log("Handling initialized notification")
    # No response needed for notifications

def handle_tools_list(request_id):
    """Handle tools/list request."""
    log("Handling tools/list")
    response = {
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "tools": [
                {
                    "name": "echo",
                    "description": "Echo back the provided text",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "text": {
                                "type": "string",
                                "description": "Text to echo back"
                            }
                        },
                        "required": ["text"]
                    }
                },
                {
                    "name": "add",
                    "description": "Add two numbers",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "a": {
                                "type": "number",
                                "description": "First number"
                            },
                            "b": {
                                "type": "number",
                                "description": "Second number"
                            }
                        },
                        "required": ["a", "b"]
                    }
                },
                {
                    "name": "fail",
                    "description": "Always returns an error",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                }
            ]
        }
    }
    print(json.dumps(response), flush=True)

def handle_tools_call(request_id, params):
    """Handle tools/call request."""
    log(f"Handling tools/call with params: {params}")
    name = params.get("name")
    arguments = params.get("arguments", {})
    
    if name == "echo":
        text = arguments.get("text", "")
        response = {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": f"Echo: {text}"
                    }
                ]
            }
        }
    elif name == "add":
        a = arguments.get("a", 0)
        b = arguments.get("b", 0)
        result = a + b
        response = {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": str(result)
                    }
                ]
            }
        }
    elif name == "fail":
        response = {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": "This tool always fails"
                    }
                ],
                "isError": True
            }
        }
    else:
        response = {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {
                "code": -32603,
                "message": f"Unknown tool: {name}"
            }
        }
    
    print(json.dumps(response), flush=True)

def main():
    """Main server loop."""
    log("Starting fake MCP server")
    request_id = 1
    
    # Send initialized notification after handshake (simplified)
    # In a real implementation, we'd wait for the initialize request first
    
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
            
        try:
            request = json.loads(line)
            log(f"Received request: {request}")
            
            # Handle notifications (no id)
            if "id" not in request:
                method = request.get("method")
                if method == "notifications/initialized":
                    handle_initialized()
                continue
            
            # Handle requests
            req_id = request.get("id", request_id)
            method = request.get("method")
            params = request.get("params", {})
            
            if method == "initialize":
                handle_initialize(req_id)
            elif method == "tools/list":
                handle_tools_list(req_id)
            elif method == "tools/call":
                handle_tools_call(req_id, params)
            else:
                # Unknown method
                error_response = {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {
                        "code": -32601,
                        "message": f"Method not found: {method}"
                    }
                }
                print(json.dumps(error_response), flush=True)
                
        except json.JSONDecodeError as e:
            log(f"Invalid JSON received: {e}")
        except Exception as e:
            log(f"Error handling request: {e}")
            # Send error response if we have an ID
            if 'request' in locals() and "id" in request:
                error_response = {
                    "jsonrpc": "2.0",
                    "id": request.get("id"),
                    "error": {
                        "code": -32603,
                        "message": f"Internal error: {str(e)}"
                    }
                }
                print(json.dumps(error_response), flush=True)

if __name__ == "__main__":
    main()